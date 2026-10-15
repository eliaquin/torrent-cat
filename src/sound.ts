type Sound =
  | "add"
  | "delete"
  | "disable"
  | "done"
  | "enable"
  | "error"
  | "play"
  | "startup";
const cache = new Map<Sound, HTMLAudioElement>();
export function playSound(name: Sound, enabled: boolean) {
  if (!enabled) return;
  let audio = cache.get(name);
  if (!audio) {
    audio = new Audio(`/sound/${name}.m4a`);
    audio.volume = ["delete", "disable", "enable"].includes(name)
      ? 0.125
      : 0.25;
    cache.set(name, audio);
  }
  audio.currentTime = 0;
  void audio.play().catch(() => {
    /* A blocked sound must not interrupt a transfer. */
  });
}
