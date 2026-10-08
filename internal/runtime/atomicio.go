package runtime

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

// writeFileAtomic writes data to path via a temp file in the same directory
// followed by rename, so readers never observe a half written file
// (core/resources/src/atomic.rs discipline). The file is created with mode.
func writeFileAtomic(path string, data []byte, mode os.FileMode) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return fmt.Errorf("创建临时文件: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName) // no-op after a successful rename
	if err := tmp.Chmod(mode); err != nil {
		tmp.Close()
		return fmt.Errorf("设置临时文件权限: %w", err)
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("写入临时文件: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("刷盘临时文件: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("关闭临时文件: %w", err)
	}
	if err := os.Rename(tmpName, path); err != nil {
		return fmt.Errorf("原子替换 %s: %w", filepath.Base(path), err)
	}
	syncDirectory(dir)
	return nil
}

// syncDirectory best-effort flushes a directory entry so a freshly created
// file survives a crash (core/resources/src/journal.rs:1932 performs the
// same step under the append lock; here it is advisory).
func syncDirectory(dir string) {
	f, err := os.Open(dir)
	if err != nil {
		return
	}
	defer f.Close()
	_ = f.Sync()
}

// fileLock is an advisory cross-process exclusive lock on a lock file,
// standing in for the Rust append.lock (core/resources/src/journal.rs:754).
// It serializes open/replay and append so a concurrent writer's half
// written line is never misjudged as corruption, and two app instances
// never interleave writes to one journal. Uses syscall.Flock, available on
// the Darwin and Linux targets of the v1 desktop build.
type fileLock struct {
	file *os.File
}

// openFileLock opens (creating if needed) the lock file at path.
func openFileLock(path string) (*fileLock, error) {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("打开追加锁: %w", err)
	}
	return &fileLock{file: file}, nil
}

// lock takes the exclusive lock, blocking until it is available.
func (l *fileLock) lock() error {
	if err := syscall.Flock(int(l.file.Fd()), syscall.LOCK_EX); err != nil {
		return fmt.Errorf("获取追加锁: %w", err)
	}
	return nil
}

// unlock releases the exclusive lock.
func (l *fileLock) unlock() {
	_ = syscall.Flock(int(l.file.Fd()), syscall.LOCK_UN)
}

// close releases the lock and the underlying descriptor.
func (l *fileLock) close() error {
	l.unlock()
	return l.file.Close()
}
