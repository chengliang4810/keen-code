package tools

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	"keencode/internal/model"
)

// Shared Read behavior (filesystem.rs:25-40, environment.rs:15).
const (
	// defaultReadLines is the page size when a Read call omits limit.
	defaultReadLines = 2_000
	// readBufferBytes bounds the fixed read window for text and skips.
	readBufferBytes = 8 << 10
	// readOnlyWallClock is the outer wall clock of the read-only tools; a
	// hung traversal or read must not stall the turn until the user
	// cancels.
	readOnlyWallClock = 15 * time.Second
)

// errLineTooLarge marks a single line that cannot fit the Read output
// budget (filesystem.rs:771-774).
var errLineTooLarge = errors.New("read_line_too_large")

// utf8BOM is the byte-order mark Read strips from the first line and Edit
// and Write preserve.
var utf8BOM = []byte{0xEF, 0xBB, 0xBF}

// readBoundary remembers where a rendered line started in the output so
// finalizeContinuation can drop whole lines (filesystem.rs:477, 742-769).
type readBoundary struct {
	sizeBefore int
	lineNumber int
}

// ReadTool reads UTF-8 text files with one-based line numbers and paged
// output (filesystem.rs:42-111). Image inlining is a recorded v1
// divergence: the plan's ToolOutput is text-only, so image files surface
// as ordinary binary-content rejections.
type ReadTool struct {
	env    *Environment
	envErr error
}

// NewRead builds the Read tool bound to one work directory.
func NewRead(workDir string) Tool {
	env, err := NewEnvironment(workDir)
	return &ReadTool{env: env, envErr: err}
}

// Definition returns the strict Read schema.
func (t *ReadTool) Definition() model.ToolDefinition {
	return model.ToolDefinition{
		Name: "Read",
		Description: "Read a UTF-8 text file with one-based line numbers; use offset and limit for pagination.\n\n" +
			"Usage:\n" +
			"- Pass an absolute path. Read the whole file when it is small; for large files read the region you need and follow the continuation offset reported by truncated output instead of guessing line numbers.\n" +
			"- Reading the same unchanged region again wastes context: after an edit, re-read only the changed area.\n" +
			"- A single line longer than the output budget cannot be returned; use Grep with context, or read a narrower region, rather than retrying the same call.\n" +
			"- Directory paths and binary files are rejected; use Glob to enumerate files.",
		InputSchema: objectSchema(map[string]any{
			"file_path": map[string]any{"type": "string", "minLength": 1},
			"offset":    map[string]any{"type": "integer", "minimum": 1},
			"limit": map[string]any{
				"type": "integer", "minimum": 1, "default": defaultReadLines,
				"description": "Maximum lines to read; byte limits may return fewer lines with a continuation offset.",
			},
		}, "file_path"),
	}
}

// Effect is always read-only: Read never mutates state.
func (t *ReadTool) Effect(json.RawMessage) Effect { return EffectReadOnly }

// Execute validates the input and streams the requested page.
func (t *ReadTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	var in readInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.FilePath) == "" {
		return errorResult("读取路径不能为空", ""), nil
	}
	if in.Offset != nil && *in.Offset < 1 {
		return errorResult("offset 必须从 1 开始", ""), nil
	}
	if in.Limit != nil && *in.Limit < 1 {
		return errorResult("limit 必须大于零", ""), nil
	}
	maxLines := t.env.Limits().MaxReadLines
	if in.Limit != nil && *in.Limit > maxLines {
		return errorResult(fmt.Sprintf("limit 不能超过 %d 行", maxLines), ""), nil
	}
	wall, cancel := context.WithTimeout(ctx, readOnlyWallClock)
	defer cancel()
	return t.read(wall, in)
}

// readInput is the strict Read input (filesystem.rs:243-252).
type readInput struct {
	FilePath string `json:"file_path"`
	Offset   *int   `json:"offset,omitempty"`
	Limit    *int   `json:"limit,omitempty"`
}

// read performs one paged file read (filesystem.rs:328-362, 438-550).
func (t *ReadTool) read(ctx context.Context, in readInput) (ToolOutput, error) {
	path, err := t.env.ResolvePath(in.FilePath)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	summary := displayPath(path)
	if err := t.env.CheckWorkspacePath(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	info, err := os.Stat(path)
	if err != nil {
		return errorResult(ioMessage("读取目标", path, err), summary), nil
	}
	if !info.Mode().IsRegular() {
		return errorResult(fmt.Sprintf("读取目标不是普通文件：%s", summary), summary), nil
	}
	limit := defaultReadLines
	if limit > t.env.Limits().MaxReadLines {
		limit = t.env.Limits().MaxReadLines
	}
	if in.Limit != nil {
		limit = *in.Limit
	}
	offset := 1
	if in.Offset != nil {
		offset = *in.Offset
	}
	content, err := readTextLines(ctx, path, offset, limit, t.env.Limits().MaxReadOutputBytes)
	if err != nil {
		if result := asResultContent(err); result != "" {
			return errorResult(result, summary), nil
		}
		return ToolOutput{}, err
	}
	return textOutput(content, summary), nil
}

// readTextLines streams one page of the file under the byte budget
// (filesystem.rs:438-550).
func readTextLines(ctx context.Context, path string, offset, limit, budget int) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", fail(ioMessage("读取目标", path, err))
	}
	defer file.Close()
	reader := bufio.NewReaderSize(file, readBufferBytes)

	if result, done := ctxResult(ctx, ""); done {
		return "", fail(result.Content)
	}
	header := "文件：" + displayPath(path) + "\n"
	if len(header) > budget {
		return "", fail("Read 输出字节上限不足以容纳固定文件头或空范围说明")
	}
	var out strings.Builder
	out.WriteString(header)

	// Skip the lines before the offset while still rejecting binary content.
	lineNumber := 0
	for i := 0; i < offset-1; i++ {
		if result, done := ctxResult(ctx, ""); done {
			return "", fail(result.Content)
		}
		more, err := skipLine(reader, path)
		if err != nil {
			return "", err
		}
		if !more {
			break
		}
		lineNumber++
	}
	if lineNumber < offset-1 {
		return "", fail(fmt.Sprintf("offset %d 超出文件末尾；文件共 %d 行", offset, lineNumber))
	}

	var boundaries []readBoundary
	for len(boundaries) < limit {
		if result, done := ctxResult(ctx, ""); done {
			return "", fail(result.Content)
		}
		current := lineNumber + 1
		line, ok, err := readVisibleLine(reader, path, budget+bomAllowance(current == 1), current == 1)
		if err != nil {
			if errors.Is(err, errLineTooLarge) {
				if len(boundaries) > 0 {
					return finalizeContinuation(out.String(), boundaries, current, budget)
				}
				// Not even the first line fits: the stable permanent error
				// (filesystem.rs:771-774).
				return "", fail("单行内容无法在 Read 输出字节上限内与必要的文件头和续读提示一起完整返回")
			}
			return "", err
		}
		if !ok {
			break
		}
		lineNumber = current
		separator := 0
		if len(boundaries) > 0 {
			separator = 1
		}
		prefix := fmt.Sprintf("%6d→", lineNumber)
		if out.Len()+separator+len(prefix)+len(line) > budget {
			return finalizeContinuation(out.String(), boundaries, lineNumber, budget)
		}
		sizeBefore := out.Len()
		if separator == 1 {
			out.WriteByte('\n')
		}
		out.WriteString(prefix)
		out.WriteString(line)
		boundaries = append(boundaries, readBoundary{sizeBefore, lineNumber})
		more, err := readerHasMore(reader, path)
		if err != nil {
			return "", err
		}
		if !more {
			return out.String(), nil
		}
	}

	if len(boundaries) == 0 {
		if out.Len()+len(emptyReadBody) > budget {
			return "", fail("Read 输出字节上限不足以容纳固定文件头或空范围说明")
		}
		out.WriteString(emptyReadBody)
		return out.String(), nil
	}
	return finalizeContinuation(out.String(), boundaries, lineNumber+1, budget)
}

// bomAllowance is the extra buffer a first line needs so the BOM (stripped
// after capture) never pushes a fitting line over the budget.
func bomAllowance(first bool) int {
	if first {
		return len(utf8BOM)
	}
	return 0
}

// finalizeContinuation drops trailing rendered lines until the continuation
// marker fits, keeping the next offset pointed at the first omitted line
// (filesystem.rs:742-769).
func finalizeContinuation(out string, boundaries []readBoundary, next, budget int) (string, error) {
	for {
		if len(boundaries) == 0 {
			return "", fail("单行内容无法在 Read 输出字节上限内与必要的文件头和续读提示一起完整返回")
		}
		marker := fmt.Sprintf("\n[仍有后续内容；下一次使用 offset=%d]", next)
		if len(out)+len(marker) <= budget {
			return out + marker, nil
		}
		last := boundaries[len(boundaries)-1]
		boundaries = boundaries[:len(boundaries)-1]
		out = out[:last.sizeBefore]
		next = last.lineNumber
	}
}

// skipLine consumes one full line discarding its content while still
// rejecting NUL bytes and invalid UTF-8 (filesystem.rs:553-589). It returns
// false at a clean end of file.
func skipLine(reader *bufio.Reader, path string) (bool, error) {
	sawBytes := false
	var pending []byte
	for {
		frag, err := reader.ReadSlice('\n')
		switch {
		case err == nil:
			if err := consumeUTF8(&pending, frag, path); err != nil {
				return false, err
			}
			if bytes.IndexByte(frag, 0) >= 0 {
				return false, fail("文本读取检测到 NUL 字节；请使用适合该二进制格式的工具")
			}
			return true, nil
		case errors.Is(err, bufio.ErrBufferFull):
			sawBytes = true
			if err := consumeUTF8(&pending, frag, path); err != nil {
				return false, err
			}
			if bytes.IndexByte(frag, 0) >= 0 {
				return false, fail("文本读取检测到 NUL 字节；请使用适合该二进制格式的工具")
			}
		case errors.Is(err, io.EOF):
			if len(frag) == 0 && !sawBytes {
				return false, nil
			}
			if err := consumeUTF8(&pending, frag, path); err != nil {
				return false, err
			}
			if len(pending) > 0 {
				return false, fail("文件不是有效 UTF-8：" + displayPath(path))
			}
			return true, nil
		default:
			return false, fail(ioMessage("读取目标", path, err))
		}
	}
}

// readVisibleLine captures one displayable line: trailing CRs before the
// newline are dropped, the first line loses its BOM, NUL bytes mark the
// file binary, and a line beyond the hard cap yields errLineTooLarge
// (filesystem.rs:592-649). It returns ok=false at a clean end of file.
func readVisibleLine(reader *bufio.Reader, path string, limit int, stripBOM bool) (line string, ok bool, err error) {
	var buf []byte
	sawBytes := false
	sawNewline := false
	for {
		frag, err := reader.ReadSlice('\n')
		switch {
		case err == nil:
			sawBytes = true
			sawNewline = true
			frag = frag[:len(frag)-1]
		case errors.Is(err, bufio.ErrBufferFull):
			sawBytes = true
		case errors.Is(err, io.EOF):
			sawBytes = sawBytes || len(frag) > 0
		default:
			return "", false, fail(ioMessage("读取目标", path, err))
		}
		if len(buf)+len(frag) > limit {
			return "", false, errLineTooLarge
		}
		buf = append(buf, frag...)
		if sawNewline || errors.Is(err, io.EOF) {
			break
		}
	}
	if !sawBytes {
		// Clean end of file before any byte of this line.
		return "", false, nil
	}
	if sawNewline {
		for len(buf) > 0 && buf[len(buf)-1] == '\r' {
			buf = buf[:len(buf)-1]
		}
	}
	if stripBOM && bytes.HasPrefix(buf, utf8BOM) {
		buf = buf[len(utf8BOM):]
	}
	if bytes.IndexByte(buf, 0) >= 0 {
		return "", false, fail("文本读取检测到 NUL 字节；请使用适合该二进制格式的工具")
	}
	if !utf8.Valid(buf) {
		return "", false, fail("文件不是有效 UTF-8：" + displayPath(path))
	}
	return string(buf), true, nil
}

// readerHasMore reports whether another byte follows the last rendered
// line (filesystem.rs:727-739).
func readerHasMore(reader *bufio.Reader, path string) (bool, error) {
	_, err := reader.Peek(1)
	switch {
	case err == nil:
		return true, nil
	case errors.Is(err, io.EOF):
		return false, nil
	default:
		return false, fail(ioMessage("读取目标", path, err))
	}
}

// consumeUTF8 appends frag to the pending buffer, consuming every complete
// valid rune and keeping only an incomplete tail for the next fragment. A
// complete-but-invalid sequence fails the read (filesystem.rs:683-704).
func consumeUTF8(pending *[]byte, frag []byte, path string) error {
	*pending = append(*pending, frag...)
	buf := *pending
	i := 0
	for i < len(buf) {
		if buf[i] < utf8.RuneSelf {
			i++
			continue
		}
		want := utf8SequenceLen(buf[i])
		if want == 0 {
			return fail("文件不是有效 UTF-8：" + displayPath(path))
		}
		if i+want > len(buf) {
			break // incomplete tail: wait for the next fragment
		}
		if !utf8.Valid(buf[i : i+want]) {
			return fail("文件不是有效 UTF-8：" + displayPath(path))
		}
		i += want
	}
	*pending = append((*pending)[:0], buf[i:]...)
	return nil
}

// utf8SequenceLen returns the encoded length a valid lead byte announces,
// or 0 for a byte that can never start a rune.
func utf8SequenceLen(lead byte) int {
	switch {
	case lead >= 0xC2 && lead <= 0xDF:
		return 2
	case lead >= 0xE0 && lead <= 0xEF:
		return 3
	case lead >= 0xF0 && lead <= 0xF4:
		return 4
	}
	return 0
}

// EditTool replaces old_string exactly, once or everywhere
// (filesystem.rs:113-177).
type EditTool struct {
	env    *Environment
	envErr error
}

// NewEdit builds the Edit tool bound to one work directory.
func NewEdit(workDir string) Tool {
	env, err := NewEnvironment(workDir)
	return &EditTool{env: env, envErr: err}
}

// Definition returns the strict Edit schema.
func (t *EditTool) Definition() model.ToolDefinition {
	return model.ToolDefinition{
		Name: "Edit",
		Description: "Replace old_string exactly in a UTF-8 file. Requires exactly one match by default; replace_all=true replaces all non-overlapping matches. Uses atomic replacement in the same directory and preserves the UTF-8 BOM, original file permissions and line-ending style (an LF old_string copied from Read output still matches a CRLF file, and replacements keep CRLF).\n\n" +
			"Usage:\n" +
			"- Read the file first and copy old_string verbatim from it, including indentation; a near-miss fails with a clear error so you can correct it in one step.\n" +
			"- Keep old_string as small as possible while still unique. When a short snippet is not unique, extend it with surrounding lines instead of setting replace_all.\n" +
			"- Use replace_all only for a deliberate global rename, and check the file afterwards.\n" +
			"- Create a new file with Write rather than Edit.",
		InputSchema: objectSchema(map[string]any{
			"file_path":   map[string]any{"type": "string", "minLength": 1},
			"old_string":  map[string]any{"type": "string", "minLength": 1},
			"new_string":  map[string]any{"type": "string"},
			"replace_all": map[string]any{"type": "boolean", "default": false},
		}, "file_path", "old_string", "new_string"),
	}
}

// Effect is always a side effect: Edit mutates files.
func (t *EditTool) Effect(json.RawMessage) Effect { return EffectSideEffect }

// Execute validates the input and performs the exact replacement.
func (t *EditTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	var in editInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.FilePath) == "" {
		return errorResult("编辑路径不能为空", ""), nil
	}
	if in.OldString == "" {
		return errorResult("old_string 不能为空", ""), nil
	}
	return t.edit(ctx, in)
}

// editInput is the strict Edit input (filesystem.rs:255-267).
type editInput struct {
	FilePath   string `json:"file_path"`
	OldString  string `json:"old_string"`
	NewString  string `json:"new_string"`
	ReplaceAll bool   `json:"replace_all,omitempty"`
}

// edit performs one exact replacement (filesystem.rs:803-913).
func (t *EditTool) edit(ctx context.Context, in editInput) (ToolOutput, error) {
	if result, done := ctxResult(ctx, ""); done {
		return result, nil
	}
	path, err := t.env.ResolvePath(in.FilePath)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	summary := displayPath(path)
	if err := rejectSymlink(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if err := t.env.CheckWorkspacePath(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	maxBytes := t.env.Limits().MaxMutationFileBytes
	info, err := os.Stat(path)
	if err != nil {
		return errorResult(ioMessage("编辑目标", path, err), summary), nil
	}
	if !info.Mode().IsRegular() {
		return errorResult(fmt.Sprintf("编辑目标不是普通文件：%s", summary), summary), nil
	}
	if info.Size() > maxBytes {
		return errorResult(fmt.Sprintf("文件大小 %d 字节，超过编辑上限 %d 字节", info.Size(), maxBytes), summary), nil
	}
	original, err := readBounded(path, maxBytes, fmt.Sprintf("文件大小超过编辑上限 %d 字节", maxBytes))
	if err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	hadBOM, originalText, err := decodeFileUTF8(original)
	if err != nil {
		return errorResult(err.Error(), summary), nil
	}
	// Read output normalizes line endings to LF; when the file consistently
	// uses CRLF and the verbatim needle misses, retry with both needles
	// promoted to CRLF so an edit never rewrites the file's line style
	// (filesystem.rs:850-866, 1146-1163).
	matchOld, matchNew := resolveLineEndingNeedles(originalText, in.OldString, in.NewString)
	matches := strings.Count(originalText, matchOld)
	if matches == 0 {
		hint := ""
		if strings.Contains(originalText, "\r\n") && strings.Contains(in.OldString, "\n") {
			hint = "；该文件包含 CRLF 行尾而 Read 输出已归一为 LF，行尾归一匹配也未命中，请核对内容"
		}
		return errorResult("old_string 在目标文件中没有精确匹配"+hint, summary), nil
	}
	if !in.ReplaceAll && matches != 1 {
		return errorResult(fmt.Sprintf("old_string 精确匹配 %d 次；请扩大上下文或使用 replace_all=true", matches), summary), nil
	}
	replacementCount := 1
	if in.ReplaceAll {
		replacementCount = matches
	}
	if err := checkEditedSize(len(originalText), hadBOM, len(matchOld), len(matchNew), replacementCount, maxBytes); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	var editedText string
	if in.ReplaceAll {
		editedText = strings.ReplaceAll(originalText, matchOld, matchNew)
	} else {
		editedText = strings.Replace(originalText, matchOld, matchNew, 1)
	}
	edited := encodeFileUTF8(editedText, hadBOM)
	if bytes.Equal(edited, original) {
		return textOutput(fmt.Sprintf("文件内容未变化：%s", summary), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	// Re-read and refuse to overwrite concurrent modifications made while
	// this call was running (filesystem.rs:1243-1272).
	current, err := readBounded(path, maxBytes, fmt.Sprintf("文件在工具执行期间发生变化，已拒绝覆盖：%s", summary))
	if err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if !bytes.Equal(current, original) {
		return errorResult(fmt.Sprintf("文件在工具执行期间发生变化，已拒绝覆盖：%s", summary), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	if err := atomicWrite(path, edited, info.Mode()); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	return textOutput(fmt.Sprintf("已原子编辑 %s，替换 %d 处，写入 %d 字节", summary, replacementCount, len(edited)), summary), nil
}

// WriteTool creates or fully overwrites a UTF-8 file atomically
// (filesystem.rs:179-240).
type WriteTool struct {
	env    *Environment
	envErr error
}

// NewWrite builds the Write tool bound to one work directory.
func NewWrite(workDir string) Tool {
	env, err := NewEnvironment(workDir)
	return &WriteTool{env: env, envErr: err}
}

// Definition returns the strict Write schema.
func (t *WriteTool) Definition() model.ToolDefinition {
	return model.ToolDefinition{
		Name: "Write",
		Description: "Create or fully overwrite a UTF-8 file. Creates missing parent directories. Writes and syncs a temporary file in the target directory before atomic replacement. Overwriting an existing text file preserves its line-ending style (LF content written over a consistently-CRLF file keeps CRLF).\n\n" +
			"Usage:\n" +
			"- Writing replaces the whole file: read an existing file before overwriting it, and prefer Edit for a targeted change.\n" +
			"- Do not create documentation, README or summary files unless the user asked for them; do not add placeholder or stub content.\n" +
			"- Content is written verbatim apart from the line-ending preservation above, so include the exact final text including its trailing newline.",
		InputSchema: objectSchema(map[string]any{
			"file_path": map[string]any{"type": "string", "minLength": 1},
			"content":   map[string]any{"type": "string"},
		}, "file_path", "content"),
	}
}

// Effect is always a side effect: Write mutates files.
func (t *WriteTool) Effect(json.RawMessage) Effect { return EffectSideEffect }

// Execute validates the input and writes the full content.
func (t *WriteTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	var in writeInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.FilePath) == "" {
		return errorResult("写入路径不能为空", ""), nil
	}
	return t.write(ctx, in)
}

// writeInput is the strict Write input (filesystem.rs:270-277).
type writeInput struct {
	FilePath string `json:"file_path"`
	Content  string `json:"content"`
}

// write performs one full-file write (filesystem.rs:916-1026). Recorded v1
// divergence: the session read-fingerprint gate (environment.rs:491-529,
// write_requires_read) is deferred — the plan's per-tool constructors share
// no session state, so a gate no call could ever satisfy would make Write
// unusable; the within-call concurrency guard below keeps the destructive
// window closed instead.
func (t *WriteTool) write(ctx context.Context, in writeInput) (ToolOutput, error) {
	if result, done := ctxResult(ctx, ""); done {
		return result, nil
	}
	path, err := t.env.ResolvePath(in.FilePath)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	summary := displayPath(path)
	if err := rejectSymlink(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if err := t.env.CheckWorkspacePath(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	maxBytes := t.env.Limits().MaxMutationFileBytes
	info, err := os.Stat(path)
	exists := err == nil
	switch {
	case err == nil:
		if !info.Mode().IsRegular() {
			return errorResult(fmt.Sprintf("写入目标不是普通文件：%s", summary), summary), nil
		}
	case os.IsNotExist(err):
		exists = false
	default:
		return errorResult(ioMessage("写入目标", path, err), summary), nil
	}
	content := []byte(in.Content)
	if int64(len(content)) > maxBytes {
		return errorResult(fmt.Sprintf("写入内容大小 %d 字节，超过上限 %d 字节", len(content), maxBytes), summary), nil
	}
	var previous []byte
	mode := os.FileMode(0) // new files keep the 0600 mode os.CreateTemp makes
	if exists {
		if info.Size() > maxBytes {
			return errorResult(fmt.Sprintf("现有文件超过写入上限 %d 字节", maxBytes), summary), nil
		}
		previous, err = readBounded(path, maxBytes, fmt.Sprintf("现有文件超过写入上限 %d 字节", maxBytes))
		if err != nil {
			return errorResult(err.Error(), summary), nil
		}
		mode = info.Mode()
	}
	// Read normalizes line endings to LF; rewriting a consistently-CRLF
	// file keeps its style (filesystem.rs:1169-1196).
	content = preserveExistingNewlineStyle(previous, content)
	if bytes.Equal(previous, content) {
		return textOutput(fmt.Sprintf("文件内容未变化：%s", summary), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	if exists {
		current, err := readBounded(path, maxBytes, fmt.Sprintf("文件在工具执行期间发生变化，已拒绝覆盖：%s", summary))
		if err != nil {
			return errorResult(err.Error(), summary), nil
		}
		if !bytes.Equal(current, previous) {
			return errorResult(fmt.Sprintf("文件在工具执行期间发生变化，已拒绝覆盖：%s", summary), summary), nil
		}
	} else if err := ensurePathStillAbsent(path); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return errorResult(ioMessage("创建父目录失败", filepath.Dir(path), err), summary), nil
	}
	if result, done := ctxResult(ctx, summary); done {
		return result, nil
	}
	if err := atomicWrite(path, content, mode); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	action := "创建"
	if exists {
		action = "覆盖"
	}
	return textOutput(fmt.Sprintf("已原子%s %s，写入 %d 字节", action, summary, len(content)), summary), nil
}

// rejectSymlink refuses atomic replacement semantics on a symlink target
// (filesystem.rs:1230-1240).
func rejectSymlink(path string) error {
	info, err := os.Lstat(path)
	switch {
	case err == nil:
		if info.Mode()&os.ModeSymlink != 0 {
			return fail(fmt.Sprintf("拒绝通过符号链接修改文件：%s", displayPath(path)))
		}
		return nil
	case os.IsNotExist(err):
		return nil
	default:
		return fail(ioMessage("符号链接检查失败", path, err))
	}
}

// readBounded reads at most max bytes through a one-byte ceiling so an
// oversized file is detected instead of silently truncated
// (filesystem.rs:1275-1309).
func readBounded(path string, max int64, tooLargeMessage string) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, fail(ioMessage("读取目标", path, err))
	}
	defer file.Close()
	ceiling := max + 1
	chunk := make([]byte, readBufferBytes)
	buf := make([]byte, 0, int(min64(max, readBufferBytes)))
	for int64(len(buf)) < ceiling {
		n, err := file.Read(chunk)
		if n > 0 {
			room := ceiling - int64(len(buf))
			if int64(n) > room {
				n = int(room)
			}
			buf = append(buf, chunk[:n]...)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, fail(ioMessage("读取目标", path, err))
		}
	}
	if int64(len(buf)) > max {
		return nil, fail(tooLargeMessage)
	}
	return buf, nil
}

// ensurePathStillAbsent refuses to overwrite a path that appeared while the
// call was running (filesystem.rs:1312-1324).
func ensurePathStillAbsent(path string) error {
	if _, err := os.Lstat(path); err == nil {
		return fail(fmt.Sprintf("路径在工具执行期间被创建，已拒绝覆盖：%s", displayPath(path)))
	} else if !os.IsNotExist(err) {
		return fail(ioMessage("并发元数据检查失败", path, err))
	}
	return nil
}

// atomicWrite writes and syncs a temporary file in the target directory and
// renames it over the target (filesystem.rs:1086-1113).
func atomicWrite(path string, content []byte, mode os.FileMode) error {
	parent := filepath.Dir(path)
	tmp, err := os.CreateTemp(parent, ".keencode-*")
	if err != nil {
		return fail(ioMessage("创建临时文件失败", parent, err))
	}
	name := tmp.Name()
	discard := func() {
		tmp.Close()
		os.Remove(name)
	}
	if mode != 0 {
		if err := tmp.Chmod(mode); err != nil {
			discard()
			return fail(ioMessage("设置文件权限失败", path, err))
		}
	}
	if _, err := tmp.Write(content); err != nil {
		discard()
		return fail(ioMessage("写入目标", path, err))
	}
	if err := tmp.Sync(); err != nil {
		discard()
		return fail(ioMessage("同步目标失败", path, err))
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return fail(ioMessage("关闭临时文件失败", path, err))
	}
	if err := os.Rename(name, path); err != nil {
		os.Remove(name)
		return fail(ioMessage("替换目标失败", path, err))
	}
	return nil
}

// decodeFileUTF8 splits the BOM and validates the UTF-8 body, rejecting NUL
// bytes as binary content (filesystem.rs:1116-1129).
func decodeFileUTF8(raw []byte) (bool, string, error) {
	if bytes.IndexByte(raw, 0) >= 0 {
		return false, "", fail("精确编辑不支持包含 NUL 字节的二进制文件")
	}
	hadBOM := bytes.HasPrefix(raw, utf8BOM)
	body := raw
	if hadBOM {
		body = raw[len(utf8BOM):]
	}
	if !utf8.Valid(body) {
		return false, "", fail("文件不是有效 UTF-8")
	}
	return hadBOM, string(body), nil
}

// encodeFileUTF8 re-attaches the BOM the original file carried
// (filesystem.rs:1132-1139).
func encodeFileUTF8(text string, withBOM bool) []byte {
	out := make([]byte, 0, len(text)+bomAllowance(withBOM))
	if withBOM {
		out = append(out, utf8BOM...)
	}
	return append(out, text...)
}

// resolveLineEndingNeedles decides the needles Edit actually matches with:
// promotion to CRLF happens only when the file consistently uses CRLF, the
// verbatim needle misses, and the promoted needle hits
// (filesystem.rs:1146-1163).
func resolveLineEndingNeedles(original, oldString, newString string) (string, string) {
	if strings.Contains(original, oldString) || !strings.Contains(oldString, "\n") {
		return oldString, newString
	}
	if !consistentlyCRLF(original) {
		return oldString, newString
	}
	crlfOld := normalizeNewlinesToCRLF(oldString)
	if strings.Contains(original, crlfOld) {
		return crlfOld, normalizeNewlinesToCRLF(newString)
	}
	return oldString, newString
}

// preserveExistingNewlineStyle promotes new content to the CRLF style of
// the file it overwrites (filesystem.rs:1169-1196).
func preserveExistingNewlineStyle(previous, content []byte) []byte {
	previousText, ok := asUTF8Text(previous)
	if !ok || !consistentlyCRLF(previousText) {
		return content
	}
	contentText, ok := asUTF8Text(content)
	if !ok || !strings.Contains(contentText, "\n") {
		return content
	}
	return []byte(normalizeNewlinesToCRLF(contentText))
}

// asUTF8Text decodes bytes that contain no NUL byte as UTF-8 text.
func asUTF8Text(raw []byte) (string, bool) {
	if len(raw) == 0 || bytes.IndexByte(raw, 0) >= 0 || !utf8.Valid(raw) {
		return "", false
	}
	return string(raw), true
}

// consistentlyCRLF reports whether every \n belongs to a \r\n pair
// (filesystem.rs:1189-1191).
func consistentlyCRLF(text string) bool {
	return strings.Contains(text, "\r\n") &&
		strings.Count(text, "\n") == strings.Count(text, "\r\n")
}

// normalizeNewlinesToCRLF promotes every \n to \r\n without touching lone
// \r bytes (filesystem.rs:1194-1196).
func normalizeNewlinesToCRLF(value string) string {
	return strings.ReplaceAll(strings.ReplaceAll(value, "\r\n", "\n"), "\n", "\r\n")
}

// checkEditedSize verifies the replacement result stays within the mutation
// budget before building it (filesystem.rs:1048-1075).
func checkEditedSize(originalBytes int, hadBOM bool, oldBytes, newBytes, count int, max int64) error {
	removed := int64(oldBytes) * int64(count)
	retained := int64(originalBytes) - removed
	inserted := int64(newBytes) * int64(count)
	total := retained + inserted + int64(bomAllowance(hadBOM))
	if retained < 0 || total > max {
		return fail(fmt.Sprintf("编辑结果超过上限 %d 字节", max))
	}
	return nil
}

// decodeStrict unmarshals one tool input rejecting unknown fields and
// trailing data (the Rust serde deny_unknown_fields behavior).
func decodeStrict(raw json.RawMessage, v any) error {
	if len(raw) == 0 {
		return fail("工具输入无效：输入为空")
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return fail("工具输入无效：" + err.Error())
	}
	if dec.More() {
		return fail("工具输入无效：存在多余数据")
	}
	return nil
}

// asResultContent exposes the model-facing content of an internal failure;
// an empty string means the error is not a result carrier.
func asResultContent(err error) string {
	var carrier *errResultContent
	if errors.As(err, &carrier) {
		return carrier.content
	}
	return ""
}

// ioMessage renders a stable IO failure that never includes file content
// (filesystem.rs:1336-1338).
func ioMessage(what, path string, err error) string {
	if os.IsNotExist(err) {
		return fmt.Sprintf("%s不存在：%s", what, displayPath(path))
	}
	return fmt.Sprintf("%s：%s：%v", what, displayPath(path), err)
}

func min64(a, b int64) int64 {
	if a < b {
		return a
	}
	return b
}
