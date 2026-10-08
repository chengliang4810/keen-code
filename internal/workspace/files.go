// Package workspace provides read-only project inspection for the native
// workbench. os.Root confines traversal, including symlinks, to the project.
package workspace

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unicode/utf8"
)

const MaxPreviewBytes = 1 << 20
const MaxDirectoryEntries = 2000

type Entry struct {
	Name      string
	Path      string // relative to the project, never an absolute external path
	Directory bool
}

func CanonicalDirectory(dir string) (string, error) {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return "", err
	}
	real, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", err
	}
	info, err := os.Stat(real)
	if err != nil {
		return "", err
	}
	if !info.IsDir() {
		return "", errors.New("项目路径不是目录")
	}
	return real, nil
}

func List(dir, relative string) ([]Entry, error) {
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	folder, err := root.Open(relative)
	if err != nil {
		return nil, err
	}
	defer folder.Close()
	entries, err := folder.ReadDir(MaxDirectoryEntries + 1)
	if err != nil && err != io.EOF {
		return nil, err
	}
	if len(entries) > MaxDirectoryEntries {
		return nil, errors.New("目录超过2000项，请在系统文件管理器中查看")
	}
	items := make([]Entry, 0, len(entries))
	for _, entry := range entries {
		if entry.Name() == ".git" {
			continue
		}
		path := filepath.Join(relative, entry.Name())
		isDir := entry.IsDir()
		if entry.Type()&os.ModeSymlink != 0 {
			if info, err := root.Stat(path); err == nil {
				isDir = info.IsDir()
			}
		}
		items = append(items, Entry{entry.Name(), path, isDir})
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].Directory != items[j].Directory {
			return items[i].Directory
		}
		return strings.ToLower(items[i].Name) < strings.ToLower(items[j].Name)
	})
	return items, nil
}

func Preview(dir, relative string) (string, error) {
	root, err := os.OpenRoot(dir)
	if err != nil {
		return "", err
	}
	defer root.Close()
	file, err := root.Open(relative)
	if err != nil {
		return "", err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", errors.New("只能预览普通文件")
	}
	data, err := io.ReadAll(io.LimitReader(file, MaxPreviewBytes+1))
	if err != nil {
		return "", err
	}
	if len(data) > MaxPreviewBytes {
		return "", errors.New("文件超过1 MiB，无法预览")
	}
	if !utf8.Valid(data) || strings.ContainsRune(string(data), 0) {
		return "", errors.New("二进制或非UTF-8文件无法预览")
	}
	return string(data), nil
}
