import wasm from "../build/git_zero_worker_wasm.wasm";
import { createWorker } from "./worker.mjs";

export default createWorker(wasm);
