package kit

import (
	"github.com/egoist/mygo/ui"

	"keencode/internal/ui/theme"
)

// ErrorBanner renders the chat error banner that sits above the composer,
// independent of the input surface (zcode-chat-specs.md §1.9,
// ChatErrorBanner.tsx:187-207): a rounded-xl bordered surface row with an
// info icon, a one-line summary and a「查看详情」outline button. onDetail
// opens the full-error dialog; the dialog itself belongs to the app layer
// (dialogs are the B stream's). The bottom margin matches the original's
// mb-6 slot. The fixed over-window copy lives in the app, which decides
// the summary text. MujicaUI's chat.ErrorMessage is not adopted here: its
// built-in retry action has no runtime counterpart yet (resending would
// duplicate the user message), so the banner keeps the working detail
// flow.
func ErrorBanner(c *ui.Context, summary string, onDetail func()) *ui.Element {
	pal := P(c)
	row := ui.Row(c).
		FillWidth().
		Margin(0, 0, 24, 0). // mb-6 slot
		Padding(8, 12).      // px-3 py-2
		Gap(8).
		AlignItems(ui.Center).
		Radius(theme.RadiusXL).
		Background(pal.Surface.Over(pal.Background)).
		Border(1, pal.Border)
	row.Children(func() {
		Icon(c, IconInfo, 16).TextColor(pal.ForegroundSubtle).Shrink(0)
		ui.Text(c, summary).
			FontSize(theme.FontBase).
			FontWeight(theme.WeightMedium).
			TextColor(pal.Foreground).
			SingleLine().
			Grow(1).
			MinWidth(0)
		if onDetail != nil {
			btn := Button(c, VariantOutline, SizeSM, "查看详情")
			if btn.Clicked() {
				onDetail()
			}
		}
	})
	return row
}
