// Platform labels for the one thing dshell shows differently per OS: the
// primary shortcut modifier. macOS uses Command; Windows and Linux use Control.
//
// The main process knows the platform, but only the pages draw these labels, and
// a UI-only fact does not belong in the IPC surface. `navigator.userAgent` is
// reliable for "is this WebKit-on-macOS" and is the same signal every web app
// uses for this.

const MAC = typeof navigator !== 'undefined' && /Mac/i.test(navigator.userAgent);

/** `⌘` on macOS, `Ctrl` elsewhere. */
export const PRIMARY_MODIFIER = MAC ? '⌘' : 'Ctrl';

/** `W` → `⌘+W` / `Ctrl+W`. */
export function shortcutLabel(key: string): string {
  return `${PRIMARY_MODIFIER}+${key}`;
}
