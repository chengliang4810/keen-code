const USER_MESSAGE_BODY_SELECTOR =
  ".lobe-chat-item--user .user-msg-body";

/**
 * WebKit may insert line breaks for block and atomic-inline boundaries while
 * serializing a selection. Compare the selected glyph sequence without those
 * engine-generated breaks, then return the DOM-clipped body text so every real
 * line break in the message is preserved.
 */
export function resolveUserMessageClipboardText(
  selectedText: string,
  userBodyText: string,
): string | null {
  if (!userBodyText) return null;
  if (selectedText === userBodyText) return userBodyText;

  const withoutSelectionLineBreaks = (text: string) =>
    text.replace(/\r\n|\r|\n/g, "");

  return withoutSelectionLineBreaks(selectedText) ===
    withoutSelectionLineBreaks(userBodyText)
    ? userBodyText
    : null;
}

function selectedTextInsideElement(range: Range, element: HTMLElement): string {
  const elementRange = element.ownerDocument.createRange();
  elementRange.selectNodeContents(element);

  const clippedRange = range.cloneRange();
  if (
    range.compareBoundaryPoints(Range.START_TO_START, elementRange) < 0
  ) {
    clippedRange.setStart(
      elementRange.startContainer,
      elementRange.startOffset,
    );
  }
  if (range.compareBoundaryPoints(Range.END_TO_END, elementRange) > 0) {
    clippedRange.setEnd(elementRange.endContainer, elementRange.endOffset);
  }

  return clippedRange.toString();
}

/**
 * Return the selection clipped to one user-message body. Cross-message
 * selections are intentionally left to the browser's normal copy behavior.
 */
export function userMessageTextFromSelection(
  root: HTMLElement,
  selection: Selection | null,
): string | null {
  if (!selection || selection.isCollapsed || selection.rangeCount !== 1) {
    return null;
  }

  const range = selection.getRangeAt(0);
  const bodies = Array.from(
    root.querySelectorAll<HTMLElement>(USER_MESSAGE_BODY_SELECTOR),
  ).filter((body) => range.intersectsNode(body));
  if (bodies.length !== 1) return null;

  const bodyText = selectedTextInsideElement(range, bodies[0]!);
  return resolveUserMessageClipboardText(selection.toString(), bodyText);
}

/** Write only plain text so WebKit cannot reintroduce block wrappers on paste. */
export function writeUserMessageSelectionToClipboard(
  root: HTMLElement,
  selection: Selection | null,
  clipboard: Pick<DataTransfer, "setData">,
): boolean {
  const text = userMessageTextFromSelection(root, selection);
  if (text == null) return false;
  try {
    clipboard.setData("text/plain", text);
    return true;
  } catch {
    // Leave the native copy path intact if a WebView exposes read-only data.
    return false;
  }
}
