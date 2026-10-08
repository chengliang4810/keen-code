package kit

import "github.com/egoist/mygo/ui"

// Context menus (docs/go-migration.md §4.2 B-stream menu.go). Mygo renders
// menus with the system's native look and reports the chosen item in the
// frame after the choice (ui/menu.go:10-31); there are no per-item style
// hooks, so ZCode's drawn menu look (bg-menu, rounded-md items, key hints
// right-aligned) is intentionally not reproduced — see 有意差异 10. What
// the kit keeps is the semantics: items, separators, disabled state, and
// the Chosen dispatch.

// ContextMenu attaches build to e as its context menu; build runs when the
// menu opens and again in the frame after an item was chosen, where the
// item's Chosen reports it. It is a thin wrapper over
// (*ui.Element).ContextMenu, kept so call sites read as kit usage.
//
// Plan deviation (docs/go-migration.md §4.2 sketches ContextMenu(c, build)):
// the element the menu belongs to is required to attach it, and the
// context carries no menu state, so the signature takes the element
// instead of the context.
func ContextMenu(e *ui.Element, build func(m *ui.Menu)) *ui.Element {
	return e.ContextMenu(build)
}

// SessionRowMenu builds the context menu of a session row: 重命名, a
// separator, then 删除, dispatching to onRename/onDelete through Chosen.
// It returns the build function for ContextMenu. Copy is inline Chinese
// for v1; i18n extraction is deferred (docs/go-migration.md §7). Nil
// callbacks make the item a no-op rather than crash.
func SessionRowMenu(onRename, onDelete func()) func(m *ui.Menu) {
	return func(m *ui.Menu) {
		if m.Item("重命名").Chosen() && onRename != nil {
			onRename()
		}
		m.Separator()
		if m.Item("删除").Chosen() && onDelete != nil {
			onDelete()
		}
	}
}
