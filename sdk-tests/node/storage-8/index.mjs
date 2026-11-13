// Re-exports the previous major line of the SDK, installed beside this file, so the suite can
// import it in place of the current one (GCSM_NODE_STORAGE=8 in harness.mjs).
export {Storage} from '@google-cloud/storage';
