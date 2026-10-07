const NATIVE_SPACE_TARGET = 'button, a[href], input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="menu"], [role="button"], [role="link"], [role="slider"], [role="checkbox"], [role="radio"], [role="switch"], [role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"], [role="combobox"], [role="textbox"], [role="tab"], [role="treeitem"], [role="option"], [role="listbox"], [role="spinbutton"], [role="scrollbar"], [role="separator"][tabindex]';

/** Playback owns Space only when neither a component nor a native control does. */
export function spaceControlsPlayback(event) {
  return event.code === "Space" && !event.defaultPrevented
    && !event.target?.closest?.(NATIVE_SPACE_TARGET);
}
