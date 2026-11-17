/* @refresh reload */
import "./index.css";
import { render } from "solid-js/web";
import { MetaProvider } from "@solidjs/meta";
import "solid-devtools";

import App from "./App";

const root = document.getElementById("root")!;

// Solid appends, so clear the prerendered markup.
root.textContent = "";

render(
    () => (
        <MetaProvider>
            <App />
        </MetaProvider>
    ),
    root,
);
