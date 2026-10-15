/**
 * Scrolls only `el`'s own vertical scroller so `el` is visible. Unlike
 * `scrollIntoView`, it never touches other ancestors (the horizontal column
 * strip, `overflow: hidden` cards), which fought the column view's own
 * scrolling and left a column blank in WKWebView.
 */
export function keepVisible(el: HTMLElement | null) {
  const scroller = el?.closest<HTMLElement>(".scroll");
  if (!el || !scroller) return;
  const row = el.getBoundingClientRect();
  const view = scroller.getBoundingClientRect();
  if (row.top < view.top) scroller.scrollTop -= view.top - row.top + 6;
  else if (row.bottom > view.bottom) scroller.scrollTop += row.bottom - view.bottom + 6;
}
