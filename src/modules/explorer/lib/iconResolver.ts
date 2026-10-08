import catppuccinIconUrls from "virtual:rcode-catppuccin-icons";
import { EXT_TO_LANGUAGE_ID } from "./constants";
import * as fileIconsMod from "./fileIcons";
import * as folderIconsMod from "./folderIcons";

const catFileNames = fileIconsMod.fileNames as Record<string, string>;
const catFileExtensions = fileIconsMod.fileExtensions as Record<string, string>;
const catLanguageIds = fileIconsMod.languageIds as Record<string, string>;
const catFolderNames = folderIconsMod.folderNames as Record<string, string>;

const DEFAULT_FILE = "file";
const DEFAULT_FOLDER = "folder";
const DEFAULT_FOLDER_OPEN = "folder-open";

// Catppuccin's manifest emits names like `folder_src`/`typescript-react`, but
// the iconify export normalizes everything to hyphenated slugs.
function toIconifySlug(name: string): string {
  return name.replace(/_/g, "-");
}

function iconUrl(iconName: string): string | null {
  return catppuccinIconUrls[toIconifySlug(iconName)] ?? null;
}

function extOf(name: string): string {
  const lower = name.toLowerCase();
  const dot = lower.indexOf(".");
  if (dot === -1 || dot === lower.length - 1) return "";
  return lower.slice(dot + 1);
}

export function fileIconUrl(name: string): string {
  const lower = name.toLowerCase();

  const byName = catFileNames[lower];
  if (byName) {
    const url = iconUrl(byName);
    if (url) return url;
  }

  let ext = extOf(lower);
  while (ext) {
    const iconName = catFileExtensions[ext];
    if (iconName) {
      const url = iconUrl(iconName);
      if (url) return url;
    }
    const langId = EXT_TO_LANGUAGE_ID[ext];
    if (langId) {
      const iconByLang = catLanguageIds[langId];
      if (iconByLang) {
        const url = iconUrl(iconByLang);
        if (url) return url;
      }
    }
    const nextDot = ext.indexOf(".");
    if (nextDot === -1) break;
    ext = ext.slice(nextDot + 1);
  }

  return iconUrl(DEFAULT_FILE) ?? "";
}

export function folderIconUrl(name: string, expanded: boolean): string {
  const lower = name.toLowerCase();

  const mapped = catFolderNames[lower];
  if (mapped) {
    const slug = toIconifySlug(mapped);
    const target = expanded ? `${slug}-open` : slug;
    const url = iconUrl(target);
    if (url) return url;
  }

  return iconUrl(expanded ? DEFAULT_FOLDER_OPEN : DEFAULT_FOLDER) ?? "";
}
