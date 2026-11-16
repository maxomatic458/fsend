import type { JSX } from "solid-js";

/** A link that opens another site in a new tab. */
export function Ext(props: { href: string; children: JSX.Element }) {
    return (
        <a
            href={props.href}
            target="_blank"
            rel="noopener noreferrer"
            class="text-azure underline underline-offset-2 decoration-azure/40 hover:text-azure-hi hover:decoration-azure-hi transition-colors"
        >
            {props.children}
        </a>
    );
}
