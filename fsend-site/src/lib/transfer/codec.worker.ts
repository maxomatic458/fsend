import { BlockDecoder, loadProto } from "../proto";
import { serve, type CodecRequest } from "./codec";

await loadProto();
const decoder = new BlockDecoder();

self.onmessage = (event: MessageEvent<CodecRequest>) => {
  const response = serve(event.data, decoder);
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
