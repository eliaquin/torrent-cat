import test from "node:test";
import assert from "node:assert/strict";
import { shouldHideControls, CONTROL_IDLE_MS, resolvePlaybackEngine } from "../src/player-controls.ts";

const playing = {
  fullscreen: true, paused: false, blocked: false, hovered: false,
  focused: false, dragging: false, lastActivity: 1000, now: 1000,
};

test("fullscreen playback hides controls after two seconds of inactivity", () => {
  assert.equal(shouldHideControls({ ...playing, now: 1000 + CONTROL_IDLE_MS - 1 }), false);
  assert.equal(shouldHideControls({ ...playing, now: 1000 + CONTROL_IDLE_MS }), true);
  assert.equal(shouldHideControls({ ...playing, now: 9000, lastActivity: 8999 }), false);
});

test("windowed playback never hides its controls", () => {
  assert.equal(shouldHideControls({ ...playing, fullscreen: false, now: 9000 }), false);
});

for (const reason of ["paused", "blocked", "hovered", "focused", "dragging"]) {
  test(`controls stay accessible while ${reason}`, () => {
    assert.equal(shouldHideControls({ ...playing, now: 9000, [reason]: true }), false);
  });
}

test("VLC preference falls back when unavailable and resumes when installed", () => {
  assert.equal(resolvePlaybackEngine("vlc", false), "builtin");
  assert.equal(resolvePlaybackEngine("vlc", true), "vlc");
});

test("explicit built-in and external player choices are preserved", () => {
  for (const available of [true, false]) {
    assert.equal(resolvePlaybackEngine("builtin", available), "builtin");
    assert.equal(resolvePlaybackEngine("external", available), "external");
  }
});
