/* The Tauri modules the app imports, as the demo build resolves them
   (demo/vite.config.js): every call is answered in the page by the demo's
   shell (bridge.js), and nothing leaves it. */
import { invoke as answer, listen as subscribe } from "./bridge.js";

/* @tauri-apps/api/core */
export const invoke = answer;
export const isTauri = () => false;
/** The app's artwork is the demo's own files, next to the page. */
export const convertFileSrc = (path) => new URL(path, document.baseURI).href;

/* @tauri-apps/api/event */
export const listen = async (event, handler) => subscribe(event, handler);

/* @tauri-apps/plugin-opener: the demo's links go nowhere outside it. */
export const openUrl = async () => {};

/* @tauri-apps/api/app, plugin-updater and plugin-process, for Settings'
   update control, which the demo does not open. */
export const getVersion = async () => "demo";
export const relaunch = async () => {};
export class Update {}
