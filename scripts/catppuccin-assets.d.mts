import type { Plugin } from "vite";

export interface CatppuccinIconSet {
  icons: Record<string, { body: string }>;
  aliases?: Record<string, { parent: string }>;
  width?: number;
  height?: number;
}

export function createCatppuccinAssets(iconSet: CatppuccinIconSet): {
  svgs: Map<string, string>;
  aliases: Map<string, string>;
};
export function catppuccinAssetsPlugin(): Plugin;
