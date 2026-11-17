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

    // hasFileSystemAccess() looks on window; without the pickers the fallback runs.
    globals.window = globals;
}

export function getDownloads() {
    return downloads;
}

export function resetDom() {
    downloads.length = 0;
    blobsByUrl.clear();
}
