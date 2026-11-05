// Tailwind v3 classes compiled in the page by UnoCSS's runtime. Claude writes
// artefacts against Tailwind 3 (bg-opacity-50, shadow, rounded), and
// preset-wind3 keeps those meanings where Tailwind 4 changed them.

import { presetWind3 } from "@unocss/preset-wind3";
import presetAnimations from "unocss-preset-animations";

const hsl = (name) => `hsl(var(--${name}))`;
const pair = (name) => ({ DEFAULT: hsl(name), foreground: hsl(`${name}-foreground`) });

export const unoConfig = {
  defaults: {
    presets: [presetWind3({ preflight: true }), presetAnimations()],
    theme: {
      colors: {
        border: hsl("border"),
        input: hsl("input"),
        ring: hsl("ring"),
        background: hsl("background"),
        foreground: hsl("foreground"),
        primary: pair("primary"),
        secondary: pair("secondary"),
        destructive: pair("destructive"),
        muted: pair("muted"),
        accent: pair("accent"),
        popover: pair("popover"),
        card: pair("card"),
      },
      borderRadius: {
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
    },
    rules: [
      ["animate-accordion-down", { animation: "forge-accordion-down 0.2s ease-out" }],
      ["animate-accordion-up", { animation: "forge-accordion-up 0.2s ease-out" }],
    ],
    preflights: [
      {
        getCSS: () => `
@keyframes forge-accordion-down { from { height: 0 } to { height: var(--radix-accordion-content-height) } }
@keyframes forge-accordion-up { from { height: var(--radix-accordion-content-height) } to { height: 0 } }`,
      },
    ],
  },
};
