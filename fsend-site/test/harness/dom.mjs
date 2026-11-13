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
