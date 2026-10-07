/** Relative labels follow local calendar dates, including 23/25-hour days. */
export function localDayRelation(timestamp, now = Date.now()) {
  const date = new Date(timestamp);
  const current = new Date(now);
  const day = new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  const today = new Date(current.getFullYear(), current.getMonth(), current.getDate()).getTime();
  if (day === today) return "Today";
  const yesterday = new Date(current.getFullYear(), current.getMonth(), current.getDate() - 1).getTime();
  return day === yesterday ? "Yesterday" : "";
}
