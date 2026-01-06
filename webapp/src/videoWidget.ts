import { TypedEventTarget } from "./util.ts";
import shaka from "shaka-player/dist/shaka-player.ui.js";

// Nice conservative value for how much buffering to wait for on all players
const BUFFER_THRESHOLD_SECONDS = 8;

export interface ShakaVideoPlayerOptions {
  containerEl: HTMLElement;
  videoEl: HTMLVideoElement;
  playBtn: HTMLButtonElement;
  pauseBtn: HTMLButtonElement;
  seekBar: HTMLInputElement;
  fullscreenBtn: HTMLButtonElement;
}

type VideoWidgetEventMap = {
  play: void;
  pause: void;
  seek: { time: number };
};

export class VideoWidget extends TypedEventTarget<VideoWidgetEventMap> {
  private container: HTMLElement;
  private video: HTMLVideoElement;
  private playBtn: HTMLButtonElement;
  private pauseBtn: HTMLButtonElement;
  private seekBar: HTMLInputElement;
  private fullscreenBtn: HTMLButtonElement;

  private player!: shaka.Player;
  private isUserSeeking = false;

  constructor(options: ShakaVideoPlayerOptions) {
    super();

    this.video = options.videoEl;
    this.video.controls = false;

    this.container = options.containerEl;
    this.playBtn = options.playBtn;
    this.pauseBtn = options.pauseBtn;
    this.seekBar = options.seekBar;
    this.fullscreenBtn = options.fullscreenBtn;

    this.player = new shaka.Player(this.video);

    // Optional: configure buffering or streaming options
    this.player.configure({
      streaming: {
        bufferingGoal: BUFFER_THRESHOLD_SECONDS,
      },
    });

    this.bindUI();
    this.bindVideoEvents();
  }

  public async attachSource(url: string) {
    try {
      await this.player.load(url);
      this.video.pause(); // start paused
    } catch (err) {
      console.error("Error loading Shaka source", err);
    }
  }

  public isBuffered(): boolean {
    const buffered = this.video.buffered;
    const currentTime = this.video.currentTime;

    for (let i = 0; i < buffered.length; i++) {
      if (currentTime >= buffered.start(i) && currentTime <= buffered.end(i)) {
        const bufferedSeconds = buffered.end(i) - currentTime;
        return bufferedSeconds >= BUFFER_THRESHOLD_SECONDS;
      }
    }
    return false;
  }

  public time(): number {
    return this.video.currentTime;
  }

  /* -------------------- Initialization -------------------- */

  private bindUI(): void {
    this.playBtn.addEventListener("click", () => this.emit("play", undefined));
    this.pauseBtn.addEventListener(
      "click",
      () => this.emit("pause", undefined),
    );

    this.fullscreenBtn.addEventListener("click", () => {
      this.toggleFullscreen();
    });

    this.seekBar.addEventListener("mousedown", () => {
      this.isUserSeeking = true;
    });

    this.seekBar.addEventListener("mouseup", () => {
      this.isUserSeeking = false;
    });

    this.seekBar.addEventListener("input", () => this.onSeekInput());
  }

  private bindVideoEvents(): void {
    this.video.addEventListener("timeupdate", this.syncUI);
    this.video.addEventListener("loadedmetadata", this.syncUI);

    document.addEventListener("fullscreenchange", this.onFullscreenChange);
  }

  private onFullscreenChange = (): void => {
    const isFs = this.isFullscreen();
    this.container.classList.toggle("is-fullscreen", isFs);
  };

  public async toggleFullscreen(): Promise<void> {
    if (this.isFullscreen()) {
      await document.exitFullscreen();
    } else {
      await this.container.requestFullscreen();
    }
  }

  public isFullscreen(): boolean {
    return document.fullscreenElement === this.container;
  }

  /* -------------------- Playback Controls -------------------- */

  public videoPlay(): void {
    this.video.play();
  }

  public videoPause(): void {
    this.video.pause();
  }

  public videoSeek(seconds: number): void {
    this.video.currentTime = seconds;
  }

  private onSeekInput = (): void => {
    const duration = this.video.duration;
    if (!Number.isNaN(duration)) {
      const time = (this.seekBar.valueAsNumber / 100) * duration;
      this.emit("seek", { time });
    }
  };

  /* -------------------- UI Sync -------------------- */

  private syncUI = (): void => {
    const { currentTime, duration } = this.video;
    if (Number.isNaN(duration)) return;

    if (!this.isUserSeeking) {
      this.seekBar.valueAsNumber = (currentTime / duration) * 100;
    }
  };

  /* -------------------- Utilities -------------------- */

  public destroy(): void {
    this.player.destroy();
  }
}
