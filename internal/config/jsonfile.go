package config

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
)

// maxConfigFileBytes caps how large a single configuration file may be. It
// mirrors MAX_PROVIDER_CONFIG_BYTES (apps/desktop/src/providers.rs:29, 8 MiB)
// and is applied to both providers.json and settings.json: a bigger file is
// treated as corrupt rather than read into memory.
const maxConfigFileBytes = 8 * 1024 * 1024

// readRegularFileBounded reads a private configuration file without following
// symlinks and rejects files above maxBytes. It is the Go equivalent of
// read_private_bytes_bounded (apps/desktop/src/storage.rs:125-174). The
// second return value reports whether the file exists; a missing file is not
// an error so callers can fall back to their defaults.
//
// The Rust original opens with O_NOFOLLOW to close the check-then-open race.
// The Go standard library cannot pass that flag portably, so this
// implementation narrows the window instead: Lstat before the open, Stat on
// the opened handle, and a final Lstat must all agree on "regular file with
// the same size". That detects replacement and truncation in practice; it is
// deliberately documented as a simplification, not a hard security boundary.
func readRegularFileBounded(path string, maxBytes int64, label string) ([]byte, bool, error) {
	info, err := os.Lstat(path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, false, nil
		}
		return nil, false, fmt.Errorf("检查%s失败：%s：%w", label, path, err)
	}
	if !regularFileMode(info) {
		return nil, false, fmt.Errorf("%s路径不是普通文件：%s", label, path)
	}
	if info.Size() > maxBytes {
		return nil, false, fmt.Errorf("%s超过 %d 字节：%s", label, maxBytes, path)
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, false, fmt.Errorf("打开%s失败：%s：%w", label, path, err)
	}
	defer file.Close()
	opened, err := file.Stat()
	if err != nil {
		return nil, false, fmt.Errorf("读取已打开%s元数据失败：%s：%w", label, path, err)
	}
	if !regularFileMode(opened) || opened.Size() != info.Size() {
		return nil, false, fmt.Errorf("%s在打开期间发生变化：%s", label, path)
	}
	data := make([]byte, info.Size())
	// ReadFull with an empty buffer returns io.EOF, which is the one
	// acceptable error here (zero-byte file); a short read surfaces as
	// ErrUnexpectedEOF and is treated as a concurrent modification.
	if _, err := io.ReadFull(file, data); err != nil && !errors.Is(err, io.EOF) {
		return nil, false, fmt.Errorf("读取%s失败：%s：%w", label, path, err)
	}
	final, err := os.Lstat(path)
	if err != nil {
		return nil, false, fmt.Errorf("复核%s失败：%s：%w", label, path, err)
	}
	if !regularFileMode(final) || final.Size() != info.Size() {
		return nil, false, fmt.Errorf("%s在读取期间发生变化：%s", label, path)
	}
	return data, true, nil
}

// regularFileMode reports whether info describes a plain regular file and not
// a symlink, directory, or device.
func regularFileMode(info fs.FileInfo) bool {
	return info.Mode()&os.ModeSymlink == 0 && info.Mode().IsRegular()
}

// atomicWritePrivate writes data to path by creating a unique temporary file
// in the same directory, syncing it, and renaming it over the target. It is
// the Go equivalent of atomic_write_private (apps/desktop/src/storage.rs:
// 176-216): the file always ends up mode 0600, a failed commit leaves the
// previous content untouched, and no temporary file is left behind.
func atomicWritePrivate(path string, data []byte) error {
	parent := filepath.Dir(path)
	if err := os.MkdirAll(parent, 0o755); err != nil {
		return fmt.Errorf("创建私有文件目录失败：%s：%w", parent, err)
	}
	if info, err := os.Lstat(path); err == nil {
		if !regularFileMode(info) {
			return fmt.Errorf("配置目标不是可替换的普通文件：%s", path)
		}
	} else if !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("检查配置目标失败：%s：%w", path, err)
	}
	temp, err := os.CreateTemp(parent, ".keencode-write-*")
	if err != nil {
		return fmt.Errorf("创建同目录临时文件失败：%s：%w", path, err)
	}
	tempName := temp.Name()
	committed := false
	defer func() {
		if !committed {
			temp.Close()
			os.Remove(tempName)
		}
	}()
	if err := temp.Chmod(0o600); err != nil {
		return fmt.Errorf("设置临时文件权限失败：%s：%w", path, err)
	}
	if _, err := temp.Write(data); err != nil {
		return fmt.Errorf("写入临时文件失败：%s：%w", path, err)
	}
	if err := temp.Sync(); err != nil {
		return fmt.Errorf("同步临时文件失败：%s：%w", path, err)
	}
	if err := temp.Close(); err != nil {
		return fmt.Errorf("关闭临时文件失败：%s：%w", path, err)
	}
	if err := os.Rename(tempName, path); err != nil {
		return fmt.Errorf("原子替换私有文件失败：%s：%w", path, err)
	}
	committed = true
	// The rename is the commit point; syncing the parent directory is
	// best-effort crash durability and must not fail the write.
	if dir, err := os.Open(parent); err == nil {
		dir.Sync()
		dir.Close()
	}
	return nil
}

// orderedObject builds a JSON object while preserving an explicit key order.
// encoding/json sorts map keys, but the persisted files must keep the Rust
// declaration order (schema first, unknown fields appended), so values are
// marshaled individually and concatenated manually.
type orderedObject struct {
	keys []string
	vals map[string]json.RawMessage
}

func newOrderedObject() *orderedObject {
	return &orderedObject{vals: make(map[string]json.RawMessage)}
}

// set marshals value immediately and appends it under key.
func (o *orderedObject) set(key string, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("序列化配置字段 %s 失败：%w", key, err)
	}
	o.setRaw(key, raw)
	return nil
}

// setRaw appends an already-encoded value under key.
func (o *orderedObject) setRaw(key string, raw json.RawMessage) {
	if _, exists := o.vals[key]; !exists {
		o.keys = append(o.keys, key)
	}
	o.vals[key] = raw
}

// bytes renders the object as indented JSON with two-space indentation,
// matching serde_json::to_vec_pretty used by the Rust stack.
func (o *orderedObject) bytes() ([]byte, error) {
	var compact bytes.Buffer
	compact.WriteByte('{')
	for i, key := range o.keys {
		if i > 0 {
			compact.WriteByte(',')
		}
		keyJSON, err := json.Marshal(key)
		if err != nil {
			return nil, fmt.Errorf("序列化配置字段名 %s 失败：%w", key, err)
		}
		compact.Write(keyJSON)
		compact.WriteByte(':')
		compact.Write(o.vals[key])
	}
	compact.WriteByte('}')
	var pretty bytes.Buffer
	if err := json.Indent(&pretty, compact.Bytes(), "", "  "); err != nil {
		return nil, fmt.Errorf("格式化配置 JSON 失败：%w", err)
	}
	pretty.WriteByte('\n')
	return pretty.Bytes(), nil
}

// sortedKeys returns the object's keys in sorted order so that unknown-field
// warnings and preserved extras stay deterministic regardless of map
// iteration order.
func sortedKeys(object map[string]json.RawMessage) []string {
	keys := make([]string, 0, len(object))
	for key := range object {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// isJSONNull reports whether raw is the JSON null literal.
func isJSONNull(raw json.RawMessage) bool {
	return string(bytes.TrimSpace(raw)) == "null"
}

// decodeJSONString decodes raw as a JSON string, rejecting null so required
// string fields keep serde's "explicit value required" semantics.
func decodeJSONString(raw json.RawMessage) (string, error) {
	if isJSONNull(raw) {
		return "", errors.New("字段不能为 null")
	}
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		return "", err
	}
	return value, nil
}

// decodeOptionalJSONString decodes raw as a string or null. The boolean
// reports whether the key carried a value at all is decided by the caller;
// this helper only distinguishes null (false) from a string (true).
func decodeOptionalJSONString(raw json.RawMessage) (*string, error) {
	if isJSONNull(raw) {
		return nil, nil
	}
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		return nil, err
	}
	return &value, nil
}

// containsString reports whether list contains value exactly.
func containsString(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}
