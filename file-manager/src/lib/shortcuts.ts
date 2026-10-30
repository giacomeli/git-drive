import { isMac } from "../components/Chrome";

/** Cmd on macOS, Ctrl on Linux, alone (no Option, no Shift unless `shift`). */
const mod = (e: KeyboardEvent, shift = false) => (isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey) && !e.altKey && (shift || !e.shiftKey);

/** Shows or hides the terminal: Cmd+J (Linux: Ctrl+J), as VS Code's panel. Taken
 * also from inside the terminal: on Linux, Ctrl+J never reaches the shell. */
export const isTerminalKey = (e: KeyboardEvent) => mod(e) && (e.code === "KeyJ" || e.key.toLowerCase() === "j");

/** Turns the split view on or off: Cmd+/ (Linux: Ctrl+/), by the character
 * (a Brazilian layout has "/" on another key) or by its key on a US layout. */
export const isSplitKey = (e: KeyboardEvent) => (mod(e, true) && e.key === "/") || (mod(e) && e.code === "Slash");

export const TERMINAL_KEYS = isMac ? "Cmd+J" : "Ctrl+J";
export const SPLIT_KEYS = isMac ? "Cmd+/" : "Ctrl+/";
