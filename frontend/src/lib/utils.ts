import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

// Custom semantic font-size tokens (globals.css @theme) must be registered as
// font-size classes. Without this, twMerge treats text-body/text-caption etc.
// as unrecognized conflicting text-* classes and drops BOTH when a styled
// Button's base "text-body" collides with a className "text-caption" - the
// element then falls back to the inherited default size.
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": [
        { text: ["micro", "caption", "body", "body-lg", "data", "title", "heading", "display", "display-lg"] },
      ],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
