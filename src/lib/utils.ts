import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge, validators } from "tailwind-merge";

// 自定义字级仅设置字号，覆盖控件默认字号时不移除显式行高或配色。
const mergeClasses = extendTailwindMerge<"ui-font-size">({
  extend: {
    classGroups: {
      "ui-font-size": [
        {
          "text-ui": [
            "xs",
            "sm",
            "caption",
            "base",
            "lg",
            "xl",
            validators.isNumber,
          ],
        },
      ],
    },
    conflictingClassGroups: {
      "font-size": ["ui-font-size"],
      "ui-font-size": ["font-size"],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return mergeClasses(clsx(inputs));
}

export function isMarkdownPath(path: string): boolean {
  return /\.(md|markdown|mdx)$/i.test(path);
}
