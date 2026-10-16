import test from "node:test";
import assert from "node:assert/strict";
import { shouldHideControls, CONTROL_IDLE_MS } from "../src/player-controls.ts";

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
