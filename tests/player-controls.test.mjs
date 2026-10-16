import test from "node:test";
import assert from "node:assert/strict";
import { shouldHideControls, CONTROL_IDLE_MS, resolvePlaybackEngine, createVideoClickHandler, handlePlayerEscape } from "../src/player-controls.ts";

test("Escape exits fullscreen first, then leaves windowed playback", async () => {
  let fullscreen = true;
  const calls = [];
  const actions = {
    isFullscreen: async () => fullscreen,
    exitFullscreen: async () => { calls.push("exit-fullscreen"); fullscreen = false; },
    back: () => calls.push("back"),
  };
  await handlePlayerEscape({ ...actions, fullscreen });
  assert.deepEqual(calls, ["exit-fullscreen"]);
  await handlePlayerEscape({ ...actions, fullscreen });
  assert.deepEqual(calls, ["exit-fullscreen", "back"]);
});

for (const [rendered, native] of [[false, true], [true, false]]) {
  test(`Escape stays in the video during a fullscreen transition (${rendered}/${native})`, async () => {
    const calls = [];
    await handlePlayerEscape({
      fullscreen: rendered,
      isFullscreen: async () => native,
      exitFullscreen: async () => { calls.push("exit-fullscreen"); },
      back: () => calls.push("back"),
    });
    assert.deepEqual(calls, ["exit-fullscreen"]);
  });
}

test("a failed fullscreen query does not leave playback", async () => {
  const calls = [];
  await assert.rejects(handlePlayerEscape({
    fullscreen: false,
    isFullscreen: async () => { throw new Error("Window unavailable"); },
    exitFullscreen: async () => { calls.push("exit-fullscreen"); },
    back: () => calls.push("back"),
  }), /Window unavailable/);
  assert.deepEqual(calls, []);
});

test("single video clicks toggle playback", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let paused = false;
  const handler = createVideoClickHandler(() => { paused = !paused; });
  handler.click(1);
  t.mock.timers.tick(300);
  assert.equal(paused, true);
  handler.click(1);
  t.mock.timers.tick(300);
  assert.equal(paused, false);
});

test("double-clicking for fullscreen does not toggle playback", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const toggle = t.mock.fn();
  const handler = createVideoClickHandler(toggle);
  handler.click(1);
  t.mock.timers.tick(100);
  handler.click(2);
  handler.cancel();
  t.mock.timers.tick(500);
  assert.equal(toggle.mock.callCount(), 0);
});

test("leaving the video cancels pending playback clicks", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const toggle = t.mock.fn();
  const handler = createVideoClickHandler(toggle);
  handler.click(1);
  handler.cancel();
  t.mock.timers.tick(500);
  assert.equal(toggle.mock.callCount(), 0);
});

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
