/** The one line the demo says when something is not part of it. */
export const notice = $state({ text: "", serial: 0 });

export function say(text) {
  notice.text = text;
  notice.serial += 1;
}
