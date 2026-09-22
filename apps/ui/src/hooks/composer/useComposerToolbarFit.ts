import { useLayoutEffect, useRef } from "react";

const FIT_CONTROL_SELECTORS = [
  ".composer-goal-chip",
  ".composer-plan-chip",
  ".cmm--reasoning",
  ".ctx-chip",
  ".cmm--model",
] as const;

function markCollapsePriorities(root: HTMLElement): HTMLElement[] {
  const controls: HTMLElement[] = [];
  FIT_CONTROL_SELECTORS.forEach((selector, priority) => {
    root.querySelectorAll<HTMLElement>(selector).forEach((control) => {
      control.dataset.composerCollapsePriority = String(priority);
      controls.push(control);
    });
  });
  return controls;
}

function clearFitState(root: HTMLElement) {
  delete root.dataset.composerModelIcon;
  root.style.removeProperty("--composer-model-max-width");
  root.querySelectorAll<HTMLElement>("[data-composer-collapse-priority]").forEach((control) => {
    delete control.dataset.composerCompact;
  });
}

function hasToolbarOverlap(root: HTMLElement): boolean {
  const leading = root.querySelector<HTMLElement>("[data-composer-leading-actions]");
  const trailing = root.querySelector<HTMLElement>("[data-composer-trailing-actions]");
  if (!leading || !trailing) return false;
  const rootRect = root.getBoundingClientRect();
  const leadingRect = leading.getBoundingClientRect();
  const trailingRect = trailing.getBoundingClientRect();
  return (
    leading.scrollWidth > leading.clientWidth + 1 ||
    leadingRect.right > trailingRect.left + 0.5 ||
    trailingRect.right > rootRect.right + 0.5
  );
}

/**
 * 在完整布局的 DOM 副本上按优先级压缩控件，避免真实工具栏在测量过程中闪动。
 * 这是布局投影，不改变模型、权限或 Composer 业务状态。
 */
export function fitComposerToolbar(root: HTMLElement): void {
  const available = root.querySelector<HTMLElement>("[data-composer-leading-actions]");
  const content = root.querySelector<HTMLElement>("[data-composer-trailing-actions]");
  if (!available || !content) return;

  const controls = markCollapsePriorities(root).sort(
    (left, right) =>
      Number(left.dataset.composerCollapsePriority) -
      Number(right.dataset.composerCollapsePriority),
  );
  if (!controls.length) return;

  clearFitState(root);
  markCollapsePriorities(root);
  if (!hasToolbarOverlap(root)) return;

  for (const control of controls) {
    control.dataset.composerCompact = "true";
    if (!hasToolbarOverlap(root)) return;
  }

  const model = root.querySelector<HTMLElement>(".cmm--model");
  const trigger = model?.querySelector<HTMLElement>(".cmm__trigger");
  if (!model || !trigger) return;

  const rootRect = root.getBoundingClientRect();
  const leadingRect = available.getBoundingClientRect();
  const trailingRect = content.getBoundingClientRect();
  const overflow = Math.max(
    0,
    leadingRect.right - trailingRect.left,
    trailingRect.right - rootRect.right,
  );
  const triggerWidth = trigger.getBoundingClientRect().width;
  const modelWidth = Math.max(28, triggerWidth - overflow);
  if (modelWidth < 80) {
    root.dataset.composerModelIcon = "true";
  } else if (rootRect.width > 0) {
    root.style.setProperty("--composer-model-max-width", `${modelWidth}px`);
  }
}

/** Keep the Composer toolbar fitted as its width, labels, or locale changes. */
export function useComposerToolbarFit() {
  const ref = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    const root = ref.current;
    if (!root || typeof window === "undefined") return;

    const update = () => {
      if (!root.parentElement || root.getBoundingClientRect().width <= 0) return;

      const probe = root.cloneNode(true) as HTMLDivElement;
      probe.setAttribute("aria-hidden", "true");
      probe.inert = true;
      Object.assign(probe.style, {
        position: "absolute",
        visibility: "hidden",
        pointerEvents: "none",
        width: `${root.getBoundingClientRect().width}px`,
        left: "0",
        top: "0",
      });
      root.parentElement.append(probe);
      try {
        fitComposerToolbar(probe);

        for (const key of ["composerModelIcon"] as const) {
          if (probe.dataset[key]) root.dataset[key] = probe.dataset[key];
          else delete root.dataset[key];
        }
        const modelMaxWidth = probe.style.getPropertyValue("--composer-model-max-width");
        if (modelMaxWidth) root.style.setProperty("--composer-model-max-width", modelMaxWidth);
        else root.style.removeProperty("--composer-model-max-width");

        const liveControls = markCollapsePriorities(root).sort(
          (left, right) =>
            Number(left.dataset.composerCollapsePriority) -
            Number(right.dataset.composerCollapsePriority),
        );
        const measuredControls = Array.from(
          probe.querySelectorAll<HTMLElement>("[data-composer-collapse-priority]"),
        ).sort(
          (left, right) =>
            Number(left.dataset.composerCollapsePriority) -
            Number(right.dataset.composerCollapsePriority),
        );
        liveControls.forEach((control, index) => {
          if (measuredControls[index]?.dataset.composerCompact) {
            control.dataset.composerCompact = "true";
          } else {
            delete control.dataset.composerCompact;
          }
        });
      } finally {
        probe.remove();
      }
    };

    const resizeObserver =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    resizeObserver?.observe(root);
    for (const element of root.querySelectorAll<HTMLElement>(
      "[data-composer-leading-actions], [data-composer-trailing-actions]",
    )) {
      resizeObserver?.observe(element);
    }

    const mutations = new MutationObserver(update);
    mutations.observe(root, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    update();

    return () => {
      resizeObserver?.disconnect();
      mutations.disconnect();
      clearFitState(root);
    };
  }, []);

  return ref;
}
