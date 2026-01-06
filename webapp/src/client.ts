// Client-side logic for synchronized DASH movie watching
// This script handles WebSocket communication, player control, and UI updates

import { pollUntil, TypedEventTarget } from "./util.ts";
import { VideoWidget } from "./videoWidget.ts";

// Type definitions for WebSocket message types
interface WebSocketMessage {
  type: string;
  time?: number;
  playing?: boolean;
  count?: number;
}

interface PlayerState {
  isPlaying: boolean;
  currentTime: number;
  clientCount: number;
  bufferReady: boolean;
}

// State variables
const state: PlayerState = {
  isPlaying: false,
  currentTime: 0,
  clientCount: 1,
  bufferReady: false,
};

type ConnectionEventMap = {
  init: { time: number; playing: boolean };
  play: void;
  pause: void;
  seek: { time: number };
  clientCount: { count: number };
};

export class Connection extends TypedEventTarget<ConnectionEventMap> {
  ws: WebSocket;
  intervalID: number;

  constructor(url: string) {
    super();

    this.ws = new WebSocket(url);
    console.log("Connecting to " + url);

    this.intervalID = setInterval(() => {
      this.ws.send(JSON.stringify({ type: "ping" }));
    }, 10000 + Math.random() * 1000);

    // WebSocket event handlers
    this.ws.onopen = function () {
      console.log("WebSocket connected");
    };

    this.ws.onmessage = (event) => {
      const data: WebSocketMessage = JSON.parse(event.data);

      switch (data.type) {
        case "init":
          if (data.time === undefined) {
            throw new Error("missing time in init message");
          }
          if (data.playing === undefined) {
            throw new Error("missing playing in init message");
          }
          this.emit("init", {
            "time": data.time,
            "playing": data.playing,
          });
          break;
        case "play":
          this.emit("play", undefined);
          break;
        case "pause":
          this.emit("pause", undefined);
          break;
        case "seek":
          if (data.time === undefined) {
            throw new Error("missing time in seek message");
          }
          this.emit("seek", { time: data.time });
          break;
        case "clientCount":
          if (data.count === undefined) {
            throw new Error("missing count in clientCount message");
          }
          this.emit("clientCount", { count: data.count });
          break;
        default:
          throw new Error(`Unknown message type: ${data.type}`);
      }
    };

    this.ws.onclose = () => {
      clearInterval(this.intervalID);
      console.log("WebSocket disconnected");
    };

    this.ws.onerror = function (error) {
      console.error("WebSocket error:", error);
    };
  }

  play() {
    this._send({ type: "play" });
  }

  pause() {
    this._send({ type: "pause" });
  }

  bufferReady() {
    this._send({ type: "bufferReady" });
  }

  seek(time: number) {
    this._send({ type: "seek", time });
  }

  _send(message: object): void {
    this.ws.send(JSON.stringify(message));
  }
}

document.addEventListener("DOMContentLoaded", function () {
  // DOM elements
  const statusMessageEl = document.getElementById(
    "statusMessage",
  ) as HTMLParagraphElement;
  const clientCountEl = document.getElementById(
    "clientCount",
  ) as HTMLSpanElement;
  const bufferStatusMessageEl = document.getElementById(
    "bufferStatusMessage",
  ) as HTMLParagraphElement;

  async function waitForBuffer(): Promise<void> {
    bufferStatusMessageEl.innerText = "Waiting for buffer...";
    await pollUntil(() => player.isBuffered());
    state.bufferReady = true;

    // Send buffer ready message to server
    console.log("bufferReady");
    connection.bufferReady();

    bufferStatusMessageEl.innerText = "";
  }

  // Initialize dash.js player
  const player = new VideoWidget({
    containerEl: document.getElementById("videoContainer") as HTMLDivElement,
    videoEl: document.getElementById("videoPlayer") as HTMLVideoElement,
    playBtn: document.getElementById("play") as HTMLButtonElement,
    pauseBtn: document.getElementById("pause") as HTMLButtonElement,
    seekBar: document.getElementById("seekBar") as HTMLInputElement,
    fullscreenBtn: document.getElementById("fullscreen") as HTMLButtonElement,
  });
  player.attachSource("/output/manifest.mpd");

  waitForBuffer();

  // Set up WebSocket connection - Deno handles WebSockets on the same path
  const connection = new Connection("ws://" + location.host);

  connection.on("init", (ev) => {
    // Initialize player state
    state.currentTime = ev.detail.time || 0;
    state.isPlaying = ev.detail.playing || false;

    if (state.currentTime > 0) {
      player.videoSeek(state.currentTime);
    }

    if (state.isPlaying) {
      player.videoPlay();
    } else {
      player.videoPause();
    }
  });

  connection.on("play", (_ev) => {
    // Only play if we have sufficient buffer
    if (state.bufferReady) {
      player.videoPlay();
      state.isPlaying = true;
      statusMessageEl.textContent = "Playing";
    } else {
      console.log(
        "Waiting for sufficient buffer before playing",
      );
      statusMessageEl.textContent = "Waiting for sufficient buffer...";
    }
  });

  connection.on("pause", (_ev) => {
    player.videoPause();
    state.isPlaying = false;
    statusMessageEl.textContent = "Paused for synchronization";
  });

  connection.on("seek", (ev) => {
    console.log("Seeking to " + ev.detail.time);
    player.videoSeek(ev.detail.time);
    state.currentTime = ev.detail.time;
    waitForBuffer();
  });

  connection.on("clientCount", (ev) => {
    state.clientCount = ev.detail.count;
    clientCountEl.textContent = state.clientCount.toString();
  });

  // Player event handlers
  player.on("play", () => {
    connection.play();
  });

  player.on("pause", () => {
    connection.pause();
  });

  player.on("seek", (ev) => {
    const seekTime = ev.detail.time;
    if (seekTime === null) {
      throw new Error("Invalid seek event");
    }

    // Send seek event to server
    connection.seek(seekTime);

    // Reset local buffer ready status
    statusMessageEl.textContent = "Seeking - buffering...";
  });

  // syncBtn.addEventListener("click", function () {
  //   // Force sync all players to current position
  //   const syncTime = player.time();
  //   connection.seek(syncTime);
  //   statusMessageEl.textContent = "Forcing synchronization to " +
  //     syncTime.toFixed(1) + "s";
  // });
});
