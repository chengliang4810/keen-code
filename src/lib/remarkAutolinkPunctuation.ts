/** Keep sentence punctuation outside GFM literal HTTP links. */
interface MarkdownNode {
  type: string;
  url?: string;
  value?: string;
  children?: MarkdownNode[];
  position?: { start: { offset?: number }; end: { offset?: number } };
}

export function remarkAutolinkPunctuation() {
  return (tree: MarkdownNode, file: { value: unknown }) => {
    const source = String(file.value);
    function visit(parent: MarkdownNode) {
      if (!parent.children) return;
      for (let i = 0; i < parent.children.length; i += 1) {
        const node = parent.children[i];
        const start = node.position?.start.offset;
        const end = node.position?.end.offset;
        const text = node.children?.[0];
        // Source equality distinguishes bare autolinks from explicit [links]
        // and <autolinks>, whose destinations must remain exactly as authored.
        if (
          node.type === "link" && node.url && /^https?:\/\//i.test(node.url) &&
          node.children?.length === 1 && text?.type === "text" &&
          start !== undefined && end !== undefined &&
          source.slice(start, end) === text.value && node.url === text.value
        ) {
          const suffix = /[，。；：！？、）》」』】〕〉”’][\s\S]*$/u.exec(text.value);
          if (suffix) {
            node.url = node.url.slice(0, -suffix[0].length);
            text.value = node.url;
            parent.children.splice(i + 1, 0, { type: "text", value: suffix[0] });
            i += 1;
          }
        }
        visit(node);
      }
    }
    visit(tree);
  };
}
