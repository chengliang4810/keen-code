package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"unicode/utf8"

	"keencode/internal/model"
)

// Shared search behavior (search.rs:29, 425-431).
const (
	// defaultSearchResults is the result cap when a call omits max_results.
	defaultSearchResults = 1_000
	// maxRenderedSearchBytes bounds the rendered Grep output; a single
	// matching line can be huge, and rendering past the agent budget is
	// pure memory amplification.
	maxRenderedSearchBytes = 512 << 10
)

// globTool matches relative paths against a glob while walking the search
// root once, in sorted order (search.rs:34-102).
type globTool struct {
	env    *Environment
	envErr error
}

// NewGlob builds the Glob tool bound to one work directory.
func NewGlob(workDir string) Tool {
	env, err := NewEnvironment(workDir)
	return &globTool{env: env, envErr: err}
}

// Definition returns the strict Glob schema.
func (t *globTool) Definition() model.ToolDefinition {
	return model.ToolDefinition{
		Name: "Glob",
		Description: "Find files under the specified directory while respecting built-in ignore rules (.git directories are skipped, hidden files are included). pattern is relative to the search root and uses / separators; explicitly use ** to span directories. Results are sorted by path.\n\n" +
			"Usage:\n" +
			"- Search with a specific pattern such as src/**/*.go rather than a bare * or **/*, which matches nearly everything and spends the result budget on noise.\n" +
			"- Use Glob to locate files by name and Grep to locate content; read the files it returns rather than inferring their contents.",
		InputSchema: objectSchema(map[string]any{
			"pattern":     map[string]any{"type": "string", "minLength": 1},
			"path":        map[string]any{"type": "string", "minLength": 1, "default": "."},
			"max_results": map[string]any{"type": "integer", "minimum": 1},
		}, "pattern"),
	}
}

// Effect is always read-only: Glob reads directory metadata only.
func (t *globTool) Effect(json.RawMessage) Effect { return EffectReadOnly }

// Execute validates the input and walks the search root once.
func (t *globTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	var in globInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.Pattern) == "" {
		return errorResult("搜索模式不能为空", ""), nil
	}
	if in.Path != nil && strings.TrimSpace(*in.Path) == "" {
		return errorResult("搜索路径不能为空", ""), nil
	}
	maxResults := t.env.Limits().MaxSearchResults
	if in.MaxResults != nil {
		if *in.MaxResults < 1 {
			return errorResult("max_results 必须大于零", ""), nil
		}
		if *in.MaxResults > maxResults {
			return errorResult(fmt.Sprintf("max_results 不能超过 %d", maxResults), ""), nil
		}
	}
	matcher, err := compileGlob(in.Pattern)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	wall, cancel := context.WithTimeout(ctx, readOnlyWallClock)
	defer cancel()
	return t.search(wall, in, matcher)
}

// globInput is the strict Glob input (search.rs:188-199).
type globInput struct {
	Pattern    string  `json:"pattern"`
	Path       *string `json:"path,omitempty"`
	MaxResults *int    `json:"max_results,omitempty"`
}

// search walks the root in sorted order and collects matching files
// (search.rs:333-393).
func (t *globTool) search(ctx context.Context, in globInput, matcher *regexp.Regexp) (ToolOutput, error) {
	root := "."
	if in.Path != nil {
		root = *in.Path
	}
	resolved, err := t.env.ResolvePath(root)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	summary := in.Pattern
	if err := t.env.CheckWorkspacePath(resolved); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	info, err := os.Stat(resolved)
	if err != nil && os.IsNotExist(err) {
		// A missing root is not a directory — one stable message, like the
		// old stack (search.rs:343-347).
		return errorResult(fmt.Sprintf("Glob 搜索根不是目录：%s", displayPath(resolved)), summary), nil
	} else if err != nil {
		return errorResult(ioMessage("搜索目标", resolved, err), summary), nil
	}
	if !info.IsDir() {
		return errorResult(fmt.Sprintf("Glob 搜索根不是目录：%s", displayPath(resolved)), summary), nil
	}
	limit := defaultSearchResults
	if limit > t.env.Limits().MaxSearchResults {
		limit = t.env.Limits().MaxSearchResults
	}
	if in.MaxResults != nil {
		limit = *in.MaxResults
	}

	var matches []string
	walkErrors := 0
	truncated := false
	cancelled := false
	filepath.WalkDir(resolved, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			walkErrors++
			if d != nil && d.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		if _, done := ctxResult(ctx, summary); done {
			cancelled = true
			return fs.SkipAll
		}
		if d.IsDir() {
			if d.Name() == ".git" {
				return fs.SkipDir
			}
			return nil
		}
		if !d.Type().IsRegular() {
			return nil
		}
		relative, relErr := filepath.Rel(resolved, p)
		if relErr != nil {
			relative = p
		}
		if matcher.MatchString(filepath.ToSlash(relative)) {
			if len(matches) == limit {
				truncated = true
				return fs.SkipAll
			}
			matches = append(matches, filepath.ToSlash(p))
		}
		return nil
	})
	if cancelled {
		if result, done := ctxResult(ctx, summary); done {
			return result, nil
		}
	}

	var out strings.Builder
	if len(matches) == 0 {
		out.WriteString("未找到匹配文件")
	} else {
		out.WriteString(strings.Join(matches, "\n"))
	}
	if truncated {
		fmt.Fprintf(&out, "\n[结果已截断到 %d 个文件]", limit)
	}
	if walkErrors != 0 {
		fmt.Fprintf(&out, "\n[遍历时跳过 %d 个不可读取项]", walkErrors)
	}
	return textOutput(out.String(), summary), nil
}

// grepTool searches UTF-8 text files with Go regular expressions
// (search.rs:104-186).
type grepTool struct {
	env    *Environment
	envErr error
}

// NewGrep builds the Grep tool bound to one work directory.
func NewGrep(workDir string) Tool {
	env, err := NewEnvironment(workDir)
	return &grepTool{env: env, envErr: err}
}

// Definition returns the strict Grep schema.
func (t *grepTool) Definition() model.ToolDefinition {
	return model.ToolDefinition{
		Name: "Grep",
		Description: "Search UTF-8 text files using regular expressions and built-in ignore rules (.git directories are skipped, hidden files are included). Supports content, matching-file, and per-file count output modes. With multiline=true, . can match across newlines.\n\n" +
			"Usage:\n" +
			"- Prefer files_with_matches or count to locate candidates cheaply, then read the file; use content with context_before/context_after only when you need the surrounding lines.\n" +
			"- Narrow with path or glob instead of scanning the whole workspace, and escape regex metacharacters when searching for literal text.\n" +
			"- Results are capped; when output is truncated, tighten the pattern rather than repeating the same search.",
		InputSchema: objectSchema(map[string]any{
			"pattern":          map[string]any{"type": "string", "minLength": 1},
			"path":             map[string]any{"type": "string", "minLength": 1, "default": "."},
			"glob":             map[string]any{"type": "string", "minLength": 1},
			"case_insensitive": map[string]any{"type": "boolean", "default": false},
			"multiline":        map[string]any{"type": "boolean", "default": false},
			"output_mode":      map[string]any{"type": "string", "enum": []string{"content", "files_with_matches", "count"}, "default": "content"},
			"context_before":   map[string]any{"type": "integer", "minimum": 0, "maximum": 100},
			"context_after":    map[string]any{"type": "integer", "minimum": 0, "maximum": 100},
			"max_results":      map[string]any{"type": "integer", "minimum": 1},
		}, "pattern"),
	}
}

// Effect is always read-only: Grep reads directories and text files.
func (t *grepTool) Effect(json.RawMessage) Effect { return EffectReadOnly }

// Execute validates the input and searches the collected files in order.
func (t *grepTool) Execute(ctx context.Context, inv Invocation) (ToolOutput, error) {
	if t.envErr != nil {
		return ToolOutput{}, t.envErr
	}
	if err := t.env.BindInvocation(inv); err != nil {
		return ToolOutput{}, err
	}
	var in grepInput
	if err := decodeStrict(inv.Input, &in); err != nil {
		return errorResult(err.Error(), ""), nil
	}
	if strings.TrimSpace(in.Pattern) == "" {
		return errorResult("搜索模式不能为空", ""), nil
	}
	if in.Path != nil && strings.TrimSpace(*in.Path) == "" {
		return errorResult("搜索路径不能为空", ""), nil
	}
	if in.Glob != nil && strings.TrimSpace(*in.Glob) == "" {
		return errorResult("glob 过滤器不能为空", ""), nil
	}
	maxResults := t.env.Limits().MaxSearchResults
	if in.MaxResults != nil {
		if *in.MaxResults < 1 {
			return errorResult("max_results 必须大于零", ""), nil
		}
		if *in.MaxResults > maxResults {
			return errorResult(fmt.Sprintf("max_results 不能超过 %d", maxResults), ""), nil
		}
	}
	if in.ContextBefore > 100 || in.ContextAfter > 100 {
		return errorResult("context_before 和 context_after 不能超过 100", ""), nil
	}
	switch in.OutputMode {
	case "", "content", "files_with_matches", "count":
	default:
		return errorResult("工具输入无效：output_mode 必须是 content、files_with_matches 或 count", ""), nil
	}
	if in.OutputMode != "content" && in.OutputMode != "" && (in.ContextBefore != 0 || in.ContextAfter != 0) {
		return errorResult("上下文行只适用于 output_mode=content", ""), nil
	}
	expr := in.Pattern
	flags := ""
	if in.Multiline {
		flags += "sm"
	}
	if in.CaseInsensitive {
		flags += "i"
	}
	if flags != "" {
		expr = "(?" + flags + ")" + expr
	}
	regex, err := regexp.Compile(expr)
	if err != nil {
		return errorResult(fmt.Sprintf("正则无效：%v", err), ""), nil
	}
	var filter *regexp.Regexp
	if in.Glob != nil {
		filter, err = compileGlob(*in.Glob)
		if err != nil {
			return errorResult(err.Error(), ""), nil
		}
	}
	wall, cancel := context.WithTimeout(ctx, readOnlyWallClock)
	defer cancel()
	return t.search(wall, in, regex, filter)
}

// grepInput is the strict Grep input (search.rs:214-242).
type grepInput struct {
	Pattern         string  `json:"pattern"`
	Path            *string `json:"path,omitempty"`
	Glob            *string `json:"glob,omitempty"`
	CaseInsensitive bool    `json:"case_insensitive,omitempty"`
	Multiline       bool    `json:"multiline,omitempty"`
	OutputMode      string  `json:"output_mode,omitempty"`
	ContextBefore   int     `json:"context_before,omitempty"`
	ContextAfter    int     `json:"context_after,omitempty"`
	MaxResults      *int    `json:"max_results,omitempty"`
}

// searchFileOutcome classifies one searched file (search.rs:577-586).
type searchFileOutcome int

const (
	searchMatched searchFileOutcome = iota
	searchUnmatched
	searchBinary
	searchLarge
	searchUnreadable
)

// matchAnalysis carries one file's match count and the deduplicated,
// ascending one-based matching lines (search.rs:642-647).
type matchAnalysis struct {
	matchCount    int
	matchingLines []int
}

// search aggregates the per-file matches in the requested output mode
// (search.rs:396-574). Recorded v1 divergence: the Rust implementation
// spreads files over four worker threads; the Go port searches sequentially
// and stays inside the same wall clock, per-file byte cap and cancellation
// points.
func (t *grepTool) search(ctx context.Context, in grepInput, regex, filter *regexp.Regexp) (ToolOutput, error) {
	root := "."
	if in.Path != nil {
		root = *in.Path
	}
	resolved, err := t.env.ResolvePath(root)
	if err != nil {
		return errorResult(err.Error(), ""), nil
	}
	summary := in.Pattern
	if err := t.env.CheckWorkspacePath(resolved); err != nil {
		return errorResult(err.Error(), summary), nil
	}
	info, err := os.Stat(resolved)
	if err != nil {
		return errorResult(ioMessage("搜索目标", resolved, err), summary), nil
	}
	if !info.Mode().IsRegular() && !info.IsDir() {
		return errorResult("Grep 搜索路径既不是文件也不是目录", summary), nil
	}
	limit := defaultSearchResults
	if limit > t.env.Limits().MaxSearchResults {
		limit = t.env.Limits().MaxSearchResults
	}
	if in.MaxResults != nil {
		limit = *in.MaxResults
	}
	files := collectSearchFiles(resolved, info.Mode().IsRegular())

	var rendered []string
	renderedBytes := 0
	resultCount := 0
	truncated := false
	skippedBinary := 0
	skippedLarge := 0
	skippedUnreadable := 0
	rootIsFile := info.Mode().IsRegular()

	for _, path := range files {
		if result, done := ctxResult(ctx, summary); done {
			return result, nil
		}
		if resultCount == limit {
			truncated = true
			break
		}
		if filter != nil && !filter.MatchString(globFilterPath(resolved, path, rootIsFile)) {
			continue
		}
		outcome, text, analysis := searchOneFile(path, regex, in.Multiline, t.env.Limits().MaxSearchFileBytes)
		switch outcome {
		case searchUnmatched:
			continue
		case searchBinary:
			skippedBinary++
			continue
		case searchLarge:
			skippedLarge++
			continue
		case searchUnreadable:
			skippedUnreadable++
			continue
		}
		pathDisplay := displayPath(path)
		if renderedBytes >= maxRenderedSearchBytes {
			truncated = true
			break
		}
		switch grepMode(in.OutputMode) {
		case grepContent:
			remaining := limit - resultCount
			selected := analysis.matchingLines
			if len(selected) > remaining {
				selected = selected[:remaining]
				truncated = true
			}
			resultCount += len(selected)
			block := renderGrepContent(pathDisplay, text, selected, in.ContextBefore, in.ContextAfter)
			rendered = append(rendered, block)
			renderedBytes += len(block)
		case grepFilesWithMatches:
			rendered = append(rendered, pathDisplay)
			renderedBytes += len(pathDisplay)
			resultCount++
		case grepCount:
			line := fmt.Sprintf("%s:%d", pathDisplay, analysis.matchCount)
			rendered = append(rendered, line)
			renderedBytes += len(line)
			resultCount++
		default: // content
			remaining := limit - resultCount
			selected := analysis.matchingLines
			if len(selected) > remaining {
				selected = selected[:remaining]
				truncated = true
			}
			resultCount += len(selected)
			block := renderGrepContent(pathDisplay, text, selected, in.ContextBefore, in.ContextAfter)
			rendered = append(rendered, block)
			renderedBytes += len(block)
		}
		if truncated {
			break
		}
	}
	var out strings.Builder
	if len(rendered) == 0 {
		out.WriteString("未找到匹配内容")
	} else {
		out.WriteString(strings.Join(rendered, "\n"))
	}
	if truncated {
		fmt.Fprintf(&out, "\n[结果已截断到 %d 项]", limit)
	}
	if skippedLarge != 0 || skippedBinary != 0 || skippedUnreadable != 0 {
		fmt.Fprintf(&out, "\n[跳过：超大文件 %d，二进制或非 UTF-8 文件 %d，不可读取文件 %d]",
			skippedLarge, skippedBinary, skippedUnreadable)
	}
	return textOutput(out.String(), summary), nil
}

// grepMode normalizes the omitted output mode to content.
func grepMode(mode string) string {
	if mode == "" {
		return "content"
	}
	return mode
}

const (
	grepContent          = "content"
	grepFilesWithMatches = "files_with_matches"
	grepCount            = "count"
)

// collectSearchFiles returns a single file or the sorted regular files of
// one ignore-aware walk (search.rs:741-780). Unreadable entries are skipped
// silently here and surface per file as unreadable outcomes.
func collectSearchFiles(root string, rootIsFile bool) []string {
	if rootIsFile {
		return []string{root}
	}
	var files []string
	filepath.WalkDir(root, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			if d != nil && d.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		if d.IsDir() {
			if d.Name() == ".git" {
				return fs.SkipDir
			}
			return nil
		}
		if d.Type().IsRegular() {
			files = append(files, p)
		}
		return nil
	})
	sort.Strings(files)
	return files
}

// globFilterPath builds the stable relative path the glob filter matches
// against (search.rs:795-805).
func globFilterPath(root, path string, rootIsFile bool) string {
	if rootIsFile {
		return filepath.ToSlash(filepath.Base(path))
	}
	relative, err := filepath.Rel(root, path)
	if err != nil {
		relative = path
	}
	return filepath.ToSlash(relative)
}

// searchOneFile reads one bounded file and analyzes its matches
// (search.rs:589-634).
func searchOneFile(path string, regex *regexp.Regexp, multiline bool, maxBytes int64) (searchFileOutcome, string, matchAnalysis) {
	file, err := os.Open(path)
	if err != nil {
		return searchUnreadable, "", matchAnalysis{}
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return searchUnreadable, "", matchAnalysis{}
	}
	if info.Size() > maxBytes {
		return searchLarge, "", matchAnalysis{}
	}
	data, err := io.ReadAll(io.LimitReader(file, maxBytes+1))
	if err != nil {
		return searchUnreadable, "", matchAnalysis{}
	}
	if int64(len(data)) > maxBytes {
		return searchLarge, "", matchAnalysis{}
	}
	if bytes.IndexByte(data, 0) >= 0 {
		return searchBinary, "", matchAnalysis{}
	}
	if !utf8.Valid(data) {
		return searchBinary, "", matchAnalysis{}
	}
	text := string(data)
	text = strings.TrimPrefix(text, "\uFEFF")
	analysis := analyzeGrepMatches(regex, text, multiline)
	if analysis.matchCount == 0 {
		return searchUnmatched, "", analysis
	}
	return searchMatched, text, analysis
}

// analyzeGrepMatches counts regex matches and maps them to one-based lines
// (search.rs:650-685).
func analyzeGrepMatches(regex *regexp.Regexp, text string, multiline bool) matchAnalysis {
	if !multiline {
		var analysis matchAnalysis
		for i, line := range splitGrepLines(text) {
			count := len(regex.FindAllString(line, -1))
			if count != 0 {
				analysis.matchCount += count
				analysis.matchingLines = append(analysis.matchingLines, i+1)
			}
		}
		return analysis
	}
	starts := lineStarts(text)
	var analysis matchAnalysis
	seen := map[int]bool{}
	for _, loc := range regex.FindAllStringIndex(text, -1) {
		analysis.matchCount++
		start := lineForOffset(starts, loc[0])
		end := loc[1]
		if loc[1] > loc[0] {
			end = loc[1] - 1
		}
		for line := start; line <= lineForOffset(starts, end); line++ {
			if !seen[line] {
				seen[line] = true
				analysis.matchingLines = append(analysis.matchingLines, line)
			}
		}
	}
	sort.Ints(analysis.matchingLines)
	return analysis
}

// splitGrepLines splits like Rust str::lines: on '\n', stripping one
// trailing '\r' per line, without a final empty element.
func splitGrepLines(text string) []string {
	text = strings.TrimSuffix(text, "\n")
	if text == "" {
		return nil
	}
	parts := strings.Split(text, "\n")
	for i, part := range parts {
		parts[i] = strings.TrimSuffix(part, "\r")
	}
	return parts
}

// lineStarts returns the zero-based byte offset of every line
// (search.rs:688-696).
func lineStarts(text string) []int {
	starts := []int{0}
	for i := 0; i < len(text); i++ {
		if text[i] == '\n' && i+1 < len(text) {
			starts = append(starts, i+1)
		}
	}
	return starts
}

// lineForOffset maps a byte offset to its one-based line
// (search.rs:699-704).
func lineForOffset(starts []int, offset int) int {
	pos := sort.SearchInts(starts, offset)
	if pos < len(starts) && starts[pos] == offset {
		return pos + 1
	}
	if pos == 0 {
		return 1
	}
	return pos
}

// renderGrepContent renders matched lines with deduplicated context; ':'
// marks matches and '-' marks context (search.rs:707-738).
func renderGrepContent(path, text string, matchingLines []int, before, after int) string {
	lines := splitGrepLines(text)
	matched := map[int]bool{}
	for _, line := range matchingLines {
		matched[line] = true
	}
	visible := map[int]bool{}
	var numbers []int
	for _, line := range matchingLines {
		start := line - before
		if start < 1 {
			start = 1
		}
		end := line + after
		if end > len(lines) {
			end = len(lines)
		}
		for current := start; current <= end; current++ {
			if _, seen := visible[current]; !seen {
				numbers = append(numbers, current)
			}
			visible[current] = visible[current] || matched[current]
		}
	}
	sort.Ints(numbers)
	var out strings.Builder
	out.WriteString(path)
	out.WriteString("\n")
	for i, number := range numbers {
		separator := byte('-')
		if visible[number] {
			separator = ':'
		}
		content := ""
		if number >= 1 && number <= len(lines) {
			content = lines[number-1]
		}
		fmt.Fprintf(&out, "%d%c%s", number, separator, content)
		if i < len(numbers)-1 {
			out.WriteString("\n")
		}
	}
	return out.String()
}
