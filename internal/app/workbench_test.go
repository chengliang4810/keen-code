package app

import (
	"context"
	"fmt"
	"image/png"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"
	"keencode/internal/workspace"
)

func workbenchFixture(t *testing.T) (*Services, *App, string) {
	t.Helper()
	svcs, a := newTestHarness(t, nil, func(s *Services) { seedProvider(t, s, "p1") })
	dir := t.TempDir()
	dir, err := workspace.CanonicalDirectory(dir)
	if err != nil {
		t.Fatal(err)
	}
	a.draftDir = dir
	if err := a.registerProject(dir); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(a.Close)
	return svcs, a, dir
}

func waitInspector(t *testing.T, tt *ui.Tester, a *App) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		tt.Frame()
		if len(a.inspector.tasks) == 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("inspection pending: %v", a.inspector.tasks)
}

func TestWorkbenchNavigationPinProjectAndRestart(t *testing.T) {
	svcs, a, dir := workbenchFixture(t)
	mgr, _ := svcs.Sessions.manager()
	s, err := mgr.Create(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := mgr.Rename(s.Meta().ID, "项目中的对话"); err != nil {
		t.Fatal(err)
	}
	a.refreshSessions()
	a.openSession(nil, s.Meta().ID)
	tt := ui.NewTester(a.View, 1280, 800)
	for _, label := range []string{"置顶", "项目", "对话", "设置", "项目中的对话", "打开标签页"} {
		if !tt.HasText(label) {
			t.Fatalf("missing %s: %q", label, tt.Texts())
		}
	}
	if err := tt.Click("置顶当前对话"); err != nil {
		t.Fatal(err)
	}
	if !s.Meta().Pinned {
		t.Fatal("header pin did not reach session metadata")
	}
	state, err := svcs.Settings.store.LoadWorkspaces()
	if err != nil || len(state.Projects) != 1 || state.Projects[0] != dir {
		t.Fatalf("project not persisted: %+v %v", state, err)
	}
	restarted := Root(svcs)
	defer restarted.Close()
	if len(restarted.navigation.projects) != 1 || !restarted.sessions[0].Pinned {
		t.Fatal("restart lost navigation")
	}
	if err := tt.Click("新建项目对话：" + dir); err != nil {
		t.Fatal(err)
	}
	if a.activeID != "" || a.draftDir != dir {
		t.Fatal("project draft did not switch")
	}
	a.removeProject(nil, dir)
	a.pinSession(nil, s.Meta())
	tt.Frame()
	if !tt.HasText("项目中的对话") {
		t.Fatal("removing project lost its conversation")
	}
	for _, excluded := range []string{"搜索", "自动化", "分组"} {
		if tt.HasText(excluded) {
			t.Fatalf("unexpected %s", excluded)
		}
	}
}

func TestWorkbenchFilesAndGitUseActualProject(t *testing.T) {
	_, a, dir := workbenchFixture(t)
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("%v %s", err, out)
		}
	}
	run("init", "-q")
	write := func(path, text string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, path), []byte(text), 0600); err != nil {
			t.Fatal(err)
		}
	}
	write("note.txt", "ORIGINAL\n")
	run("add", ".")
	run("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial")
	write("note.txt", "CHANGED_FROM_DISK\n")
	if err := os.Mkdir(filepath.Join(dir, "src"), 0700); err != nil {
		t.Fatal(err)
	}
	write("src/code.go", "package fixture\n")
	tt := ui.NewTester(a.View, 1280, 800)
	if err := tt.Click("面板：差异"); err != nil {
		t.Fatal(err)
	}
	waitInspector(t, tt, a)
	if !tt.HasText("+CHANGED_FROM_DISK") || !tt.HasText("-ORIGINAL") {
		t.Fatalf("actual diff missing: %q", tt.Texts())
	}
	if err := tt.Click("面板：文件"); err != nil {
		t.Fatal(err)
	}
	waitInspector(t, tt, a)
	if err := tt.Click("文件：note.txt"); err != nil {
		t.Fatal(err)
	}
	waitInspector(t, tt, a)
	if !tt.HasText("CHANGED_FROM_DISK\n") {
		t.Fatalf("actual preview missing: %q", tt.Texts())
	}
	if err := tt.Click("返回文件列表"); err != nil {
		t.Fatal(err)
	}
	if err := tt.Click("目录：src"); err != nil {
		t.Fatal(err)
	}
	waitInspector(t, tt, a)
	if err := tt.Click("文件：src/code.go"); err != nil {
		t.Fatal(err)
	}
	waitInspector(t, tt, a)
	if !tt.HasText("package fixture\n") {
		t.Fatal("nested preview missing")
	}
	other := t.TempDir()
	other, err := workspace.CanonicalDirectory(other)
	if err != nil {
		t.Fatal(err)
	}
	a.selectProject(nil, other)
	tt.Frame()
	if a.inspector.file != "" || a.inspector.dir != other || strings.Contains(a.inspector.content, "CHANGED_FROM_DISK") {
		t.Fatal("previous workspace leaked into new inspector")
	}
}

func TestWorkbenchDropsStaleInspection(t *testing.T) {
	_, a, _ := workbenchFixture(t)
	tt := ui.NewTester(a.View, 1280, 800)
	started, release := make(chan struct{}), make(chan struct{})
	a.inspector.file = "old.txt"
	a.inspect("file:old.txt", "old.txt", func(ctx context.Context) inspectionResult {
		close(started)
		<-release
		return inspectionResult{text: "OLD_WORKSPACE"}
	})
	<-started
	a.selectProject(nil, t.TempDir())
	tt.Frame()
	close(release)
	for i := 0; i < 5; i++ {
		tt.Frame()
		time.Sleep(5 * time.Millisecond)
	}
	if a.inspector.content != "" || a.inspector.file != "" {
		t.Fatal("stale result accepted")
	}
}

func TestWorkbenchConversationSwitchCancelsSameDirectoryInspection(t *testing.T) {
	svcs, a, dir := workbenchFixture(t)
	mgr, err := svcs.Sessions.manager()
	if err != nil {
		t.Fatal(err)
	}
	first, err := mgr.Create(dir)
	if err != nil {
		t.Fatal(err)
	}
	second, err := mgr.Create(dir)
	if err != nil {
		t.Fatal(err)
	}
	a.openSession(nil, first.Meta().ID)
	tt := ui.NewTester(a.View, 1280, 800)
	cancelled := make(chan struct{})
	a.inspector.file = "old.txt"
	a.inspect("file:old.txt", "old.txt", func(ctx context.Context) inspectionResult {
		<-ctx.Done()
		close(cancelled)
		return inspectionResult{text: "PREVIOUS_CONVERSATION"}
	})
	a.openSession(nil, second.Meta().ID)
	tt.Frame()
	select {
	case <-cancelled:
	case <-time.After(time.Second):
		t.Fatal("conversation switch retained the previous inspection")
	}
	tt.Frame()
	if a.inspector.owner != second.Meta().ID || a.inspector.file != "" || a.inspector.content != "" {
		t.Fatal("same-directory conversations shared inspector state")
	}
}

func TestWorkbenchLayoutAcrossThemesAndNarrowWindow(t *testing.T) {
	for _, width := range []int{720, 1280, 1920} {
		for _, dark := range []bool{false, true} {
			t.Run(fmt.Sprintf("%d/dark=%v", width, dark), func(t *testing.T) {
				_, a, _ := workbenchFixture(t)
				tt := ui.NewTester(a.View, width, 800)
				tt.SetTitleBar(ui.TitleBar{Height: 48, Left: 112})
				tt.SetDark(dark)
				main, ok := tt.Find("对话面板")
				if !ok {
					t.Fatal("missing main")
				}
				draft, _ := tt.Find("Composer")
				if main.W < conversationMinWidth || draft.X < main.X || draft.X+draft.W > main.X+main.W+1 {
					t.Fatalf("main=%#v draft=%#v", main, draft)
				}
				if width >= 1200 {
					side, ok := tt.Find("右侧面板")
					if !ok || side.W < inspectorMinWidth || side.X < main.X+main.W {
						t.Fatalf("three columns: main=%#v side=%#v", main, side)
					}
				} else {
					if err := tt.Click("展开右侧面板"); err != nil {
						t.Fatal(err)
					}
					if _, ok := tt.Find("左侧导航"); ok {
						t.Fatal("narrow pane overlaps sidebar")
					}
					main, _ = tt.Find("对话面板")
					if main.W < conversationMinWidth-8 {
						t.Fatalf("narrow main=%#v", main)
					}
					expand, ok := tt.Find("展开侧栏")
					if !ok || expand.X < 120 || expand.X+expand.W > main.X+main.W {
						t.Fatalf("header overlaps window controls: %#v", expand)
					}
					if err := tt.Click("关闭右侧面板"); err != nil {
						t.Fatal(err)
					}
					if _, ok := tt.Find("左侧导航"); !ok {
						t.Fatal("closing side didn't restore sidebar")
					}
				}
				if out := os.Getenv("KEENCODE_VISUAL_OUTPUT"); out != "" {
					if err := os.MkdirAll(out, 0755); err != nil {
						t.Fatal(err)
					}
					f, err := os.Create(filepath.Join(out, fmt.Sprintf("workbench-%d-dark-%v.png", width, dark)))
					if err != nil {
						t.Fatal(err)
					}
					defer f.Close()
					if err := png.Encode(f, tt.Image()); err != nil {
						t.Fatal(err)
					}
				}
			})
		}
	}
}

func TestProjectSaveFailurePreservesNavigation(t *testing.T) {
	_, a, dir := workbenchFixture(t)
	path := filepath.Join(a.svcs.Settings.store.Root(), "workspaces.json")
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(path, 0700); err != nil {
		t.Fatal(err)
	}
	if err := a.registerProject(t.TempDir()); err == nil {
		t.Fatal("expected persist failure")
	}
	if len(a.navigation.projects) != 1 || a.navigation.projects[0] != dir {
		t.Fatal("failed save altered project list")
	}
}

func TestWorkbenchCloseDoesNotRestartHiddenTerminal(t *testing.T) {
	_, a, dir := workbenchFixture(t)
	a.resetInspector(dir)
	a.inspector.tab = "终端"
	a.inspector.termError = "fixture startup failure"
	tt := ui.NewTester(a.View, 1280, 800)
	if err := tt.Click("关闭右侧面板"); err != nil {
		t.Fatal(err)
	}
	if !a.inspector.hidden || len(a.inspector.tasks) != 0 {
		t.Fatal("closing the pane started a hidden terminal")
	}
}
