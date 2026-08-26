/**
 * File drag-drop zone hit testing: add-project panel vs main attachments.
 *
 * Project folders are accepted only by the visible source-folder control.
 *
 * Coordinate note: Tauri types drag positions as PhysicalPosition, but on
 * macOS wry reports NSDraggingInfo.draggingLocation() which is already in
 * view points (logical). Dividing those by scaleFactor expands the left zone
 * to roughly half the window on Retina. Windows ScreenToClient is physical.
 */

export type DragZone = "project" | "main" | null;

export interface RectLike {
  left: number;
  right: number;
  top: number;
  bottom: number;
  width: number;
}

/**
 * Whether Tauri drag positions need / scaleFactor to match CSS client coords.
 * mac / iOS: already logical points. win / other: physical pixels.
 */
export function dragPosNeedsScale(platform: string): boolean {
  return platform === "win" || platform === "linux" || platform === "other";
}

/** Convert a Tauri drag position into CSS client coordinates. */
export function toClientDragPoint(
  pos: { x: number; y: number },
  scaleFactor: number,
  platform: string,
): { x: number; y: number } {
  if (!dragPosNeedsScale(platform)) {
    return { x: pos.x, y: pos.y };
  }
  const f = scaleFactor > 0 ? scaleFactor : 1;
  return { x: pos.x / f, y: pos.y / f };
}

/**
 * When the add-project panel is closed, every file drop is an attachment.
 * While it is open, only its source-folder control accepts a project folder;
 * dropping elsewhere is ignored so the modal cannot mutate content behind it.
 */
export function hitDragZoneFromRects(
  clientX: number,
  clientY: number,
  projectDrop: RectLike | null,
  addProjectOpen: boolean,
): DragZone {
  if (!addProjectOpen) return "main";
  if (!projectDrop || projectDrop.width < 2) return null;
  if (
    clientX >= projectDrop.left &&
    clientX < projectDrop.right &&
    clientY >= projectDrop.top &&
    clientY <= projectDrop.bottom
  ) {
    return "project";
  }
  return null;
}
