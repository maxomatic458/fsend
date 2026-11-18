/** public/brand/logo.svg, or its all-flame variant. Decorative, and not draggable so it is never dropped as a file. */
export function Logo(props: { class?: string; tint?: "brand" | "flame" }) {
    return (
        <img
            src={props.tint === "flame" ? "/brand/logo-flame.svg" : "/brand/logo.svg"}
            alt=""
            draggable={false}
            class={props.class}
        />
    );
}
