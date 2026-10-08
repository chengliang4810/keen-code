package app

import (
	"fmt"
	"image/png"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/egoist/mygo/ui"
	"keencode/internal/config"
	"keencode/internal/ui/theme"
)

func TestComposerSelectionAfterModelReplacement(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	record, err := config.NewProviderRecord("p1", "测试供应商", "https://api.test.local/v1", config.ProtocolMessages, []string{"replacement-model"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := svcs.Settings.UpsertProvider(record); err != nil {
		t.Fatal(err)
	}
	a.refreshModelItems()
	a.applyModelSelection(nil)
	if a.modelSel != encodeModelValue("p1", "replacement-model") {
		t.Fatalf("deleted model remains selected: %q", a.modelSel)
	}
	store, _ := config.OpenStore(svcs.Settings.store.Root())
	state, _, err := store.LoadProviders()
	if err != nil {
		t.Fatal(err)
	}
	if model, _ := state.ActiveModel(); model != "replacement-model" {
		t.Fatalf("persisted selection = %q", model)
	}
}

func TestComposerLayoutAcrossThemesAndWindowSizes(t *testing.T) {
	for _, size := range [][2]int{{720, 480}, {1280, 800}, {1920, 1080}} {
		for _, dark := range []bool{false, true} {
			t.Run(fmt.Sprintf("%dx%d/dark=%v", size[0], size[1], dark), func(t *testing.T) {
				_, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
				tt := ui.NewTester(a.View, size[0], size[1])
				tt.SetDark(dark)
				get := func(label string) ui.Rect {
					r, ok := tt.Find(label)
					if !ok {
						t.Fatalf("missing %s", label)
					}
					return r
				}
				shell, draft, send, greeting := get("Composer"), get("Draft"), get("Send"), get(Greeting)
				if draft.H < 40 || draft.H > 80 {
					t.Fatalf("empty editor line height: %#v", draft)
				}
				if send.X < shell.X || send.Y < shell.Y || send.X+send.W > shell.X+shell.W || send.Y+send.H > shell.Y+shell.H {
					t.Fatalf("send %#v escapes shell %#v", send, shell)
				}
				if shell.Y < greeting.Y+greeting.H+30 || shell.Y+shell.H > float32(size[1])-16 {
					t.Fatalf("greeting/composer grouping: greeting=%#v composer=%#v", greeting, shell)
				}
				if greeting.Y < 48+float32(size[1])*0.29-2 || greeting.Y > 48+float32(size[1])*0.29+8 {
					t.Fatalf("draft must follow ZCode's 29dvh top basis: %#v", greeting)
				}
				if out := os.Getenv("KEENCODE_VISUAL_OUTPUT"); out != "" {
					if err := os.MkdirAll(out, 0755); err != nil {
						t.Fatal(err)
					}
					f, err := os.Create(filepath.Join(out, fmt.Sprintf("draft-%dx%d-dark-%v.png", size[0], size[1], dark)))
					if err != nil {
						t.Fatal(err)
					}
					err = png.Encode(f, tt.Image())
					_ = f.Close()
					if err != nil {
						t.Fatal(err)
					}
				}
				if err := tt.Click("Draft"); err != nil {
					t.Fatal(err)
				}
				tt.Type(strings.Repeat("long draft\n", 100))
				if r := get("Draft"); r.H > theme.SpaceUnit*40+0.01 {
					t.Fatalf("long input grows beyond cap: %#v", r)
				}
				if r := get("Send"); r.Y+r.H > float32(size[1])-16 {
					t.Fatalf("long input hides Send: %#v", r)
				}
			})
		}
	}
}

func TestComposerSelectionAfterLastProviderDeleted(t *testing.T) {
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	if err := svcs.Settings.DeleteProvider("p1"); err != nil {
		t.Fatal(err)
	}
	a.refreshModelItems()
	if a.modelReady() || a.canSend("hello", false) {
		t.Fatal("composer still accepts sending with a deleted provider")
	}
	tt := ui.NewTester(a.View, 1100, 720)
	if !tt.HasText(ManageModelsLabel) {
		t.Fatal("no route to configure a model")
	}
}

func TestFailedProviderReplacementKeepsCurrentModel(t *testing.T) {
	svcs, _ := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	before := svcs.Settings.Providers()
	path := filepath.Join(svcs.Settings.store.Root(), "providers.json")
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(path, 0700); err != nil {
		t.Fatal(err)
	}
	replacement, err := config.NewProviderRecord("p1", "replacement", "https://api.test.local/v1", config.ProtocolMessages, []string{"new-model"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := svcs.Settings.UpsertProvider(replacement); err == nil {
		t.Fatal("expected failed persistence")
	}
	after := svcs.Settings.Providers()
	if !reflect.DeepEqual(before, after) {
		t.Fatal("failed save replaced the current provider/model in memory")
	}
}
