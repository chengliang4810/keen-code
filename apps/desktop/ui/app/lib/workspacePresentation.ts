export function workspacePresentationId(
  activeId: number,
  toolsOpen: boolean,
  toolView: string,
  ready: boolean,
): number {
  return toolsOpen && toolView === "workspace" && ready ? activeId : -1;
}
