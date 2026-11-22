import { loadProto } from "../proto";
import { codecState, serve, type CodecRequest } from "./codec";

await loadProto();
const state = codecState();

self.onmessage = (event: MessageEvent<CodecRequest>) => {
    const response = serve(event.data, state);
    const transfer =
        "frame" in response
            ? [response.frame.buffer]
            : "blocks" in response
              ? response.blocks.map((block) => block.buffer)
              : [];
    self.postMessage(response, { transfer });
};

// Ready for requests.
self.postMessage(null);
