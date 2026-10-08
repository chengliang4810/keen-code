package kit

import (
	"embed"

	"github.com/egoist/mygo/ui"
)

// The SVG sources are the lucide icons ZCode uses (lucide-static v1.52.0,
// ISC license © Lucide Contributors, https://lucide.dev). Each file keeps
// its upstream license header comment; the ISC license accompanies the
// SVG directory. The original chat icons and seven workbench icons are
// embedded; scope does not extend beyond these files.

//go:embed svg/*.svg
var svgFS embed.FS

// icons holds one parsed SVG per icon, registered once at package init so
// the view never parses (ui/svg.go:49-57 recommends parsing once).
var icons = func() map[IconName]*ui.SVG {
	files := map[IconName]string{
		IconArrowUp:      "arrow-up.svg",
		IconArrowDown:    "arrow-down.svg",
		IconSquare:       "square.svg",
		IconBrain:        "brain.svg",
		IconChevronRight: "chevron-right.svg",
		IconChevronDown:  "chevron-down.svg",
		IconCopy:         "copy.svg",
		IconCheck:        "check.svg",
		IconPlus:         "plus.svg",
		IconGear:         "settings.svg",
		IconInfo:         "info.svg",
		IconAlert:        "triangle-alert.svg",
		IconTerminal:     "terminal.svg",
		IconFile:         "file.svg",
		IconFolder:       "folder.svg",
		IconFolderOpen:   "folder-open.svg",
		IconPin:          "pin.svg",
		IconPanelLeft:    "panel-left.svg",
		IconPanelRight:   "panel-right.svg",
		IconFileDiff:     "file-diff.svg",
		IconRefresh:      "refresh-cw.svg",
		IconPencil:       "pencil.svg",
		IconTrash:        "trash.svg",
		IconX:            "x.svg",
	}
	m := make(map[IconName]*ui.SVG, len(files))
	for name, file := range files {
		data, err := svgFS.ReadFile("svg/" + file)
		if err != nil {
			panic("kit: embedded icon " + file + ": " + err.Error())
		}
		m[name] = ui.MustParseSVG(data)
	}
	return m
}()

// loaderCircle is the lucide "loader-circle" the chat loading spinner
// turns (zcode-chat-specs.md §1.10, chat-loading.tsx:21-36).
var loaderCircle = mustIcon("loader-circle.svg")

func mustIcon(file string) *ui.SVG {
	data, err := svgFS.ReadFile("svg/" + file)
	if err != nil {
		panic("kit: embedded icon " + file + ": " + err.Error())
	}
	return ui.MustParseSVG(data)
}

// Icon returns the named icon at sizeDIP DIP, drawn in the color of the
// surrounding text (ui.Icon takes TextColor from the ancestors). Give the
// element a TextColor to tint it; unknown names panic, which keeps the
// IconName contract honest at the first frame.
func Icon(c *ui.Context, name IconName, sizeDIP float32) *ui.Element {
	svg, ok := icons[name]
	if !ok {
		panic("kit: unknown icon " + string(name))
	}
	// An icon is as high as its font size (ui/svg.go:71-88), so the size
	// rides on FontSize. Shrink keeps it from stretching in a row.
	return ui.Icon(c, svg).FontSize(sizeDIP).Shrink(0)
}
