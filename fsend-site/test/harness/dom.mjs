/**
 * The sliver of DOM the fallback (no File System Access) receiver needs to
 * hand a finished download to the user: an object URL and an anchor click.
 * Downloads are captured so tests can assert on the bytes that would have
 * been saved.
 */

const downloads = [];
const blobsByUrl = new Map();
let urlCounter = 0;

export function installDom(globals) {
  const objectUrl = {
    createObjectURL(blob) {
      const url = `blob:fsend-test/${++urlCounter}`;
      blobsByUrl.set(url, blob);
      return url;
    },
    revokeObjectURL(url) {
      blobsByUrl.delete(url);
    },
  };

  // Keep the real URL class; just add the two static helpers.
  globals.URL.createObjectURL = objectUrl.createObjectURL;
  globals.URL.revokeObjectURL = objectUrl.revokeObjectURL;

  globals.document = {
    body: { appendChild() {}, removeChild() {} },
    createElement(tag) {
      if (tag !== "a") return { style: {} };
      const anchor = {
        tag,
        href: "",
        download: "",
        style: {},
        click() {
          downloads.push({
            name: anchor.download,
            blob: blobsByUrl.get(anchor.href) ?? null,
          });
        },
      };
      return anchor;
    },
  };

  // jszip reads Blob input through FileReader, which Node has no equivalent
  // of. Without this the zip path can't be tested at all.
  globals.FileReader = class FileReader {
    constructor() {
      this.result = null;
      this.error = null;
      this.onload = null;
      this.onerror = null;
    }
    readAsArrayBuffer(blob) {
      blob.arrayBuffer().then(
        (buf) => {
          this.result = buf;
          this.onload?.({ target: this });
        },
        (err) => {
          this.error = err;
          this.onerror?.({ target: this });
        },
      );
    }
  };

  // supportsFileSystemAccess() checks for these on window; the fallback path
  // is the one that must run when they are absent.
  globals.window = globals;
}

export function getDownloads() {
  return downloads;
}

export function resetDom() {
  downloads.length = 0;
  blobsByUrl.clear();
}
