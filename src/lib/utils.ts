import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merge Tailwind classes for product-specific Appica compositions. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
