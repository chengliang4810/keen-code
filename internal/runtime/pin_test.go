package runtime

import "testing"

func TestPinnedSessionPersistsWithoutChangingConversation(t *testing.T) {
	root := t.TempDir()
	mgr, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	s, err := mgr.Create(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	before := s.Meta()
	if err := mgr.SetPinned(before.ID, true); err != nil {
		t.Fatal(err)
	}
	if !s.Meta().Pinned || s.Meta().UpdatedAt != before.UpdatedAt {
		t.Fatal("pin changed conversation ordering or not applied")
	}
	fresh, err := OpenManager(root, ManagerOptions{})
	if err != nil {
		t.Fatal(err)
	}
	list, err := fresh.List()
	if err != nil || len(list) != 1 || !list[0].Pinned {
		t.Fatalf("restart=%+v err=%v", list, err)
	}
	if err := fresh.SetPinned(before.ID, false); err != nil {
		t.Fatal(err)
	}
	loaded, err := fresh.Get(before.ID)
	if err != nil || loaded.Meta().Pinned {
		t.Fatal("unpin did not persist")
	}
}
