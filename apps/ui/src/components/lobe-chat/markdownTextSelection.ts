const markdownTextBlockSelector = "p,li,h1,h2,h3,h4,h5,h6,td,th";

export function findMarkdownTextBlock(target: Element): HTMLElement | null {
  return target.closest<HTMLElement>(markdownTextBlockSelector);
}

export function selectMarkdownTextBlock(
  target: HTMLElement,
  clickCount: number,
): boolean {
  if (clickCount !== 3) return false;
  const selection = target.ownerDocument.getSelection();
  if (!selection) return false;
  const walker = target.ownerDocument.createTreeWalker(target, 4);
  const textNodes: Text[] = [];
  let node = walker.nextNode();
  while (node) {
    const text = node as Text;
    if (
      node.nodeType === 3 &&
      text.data.trim() &&
      text.parentElement?.closest(markdownTextBlockSelector) === target
    ) {
      textNodes.push(text);
    }
    node = walker.nextNode();
  }
  const first = textNodes[0];
  const last = textNodes.at(-1);
  if (!first || !last) return false;
  const range = target.ownerDocument.createRange();
  range.setStart(first, first.data.length - first.data.trimStart().length);
  range.setEnd(last, last.data.trimEnd().length);
  selection.removeAllRanges();
  selection.addRange(range);
  return true;
}
