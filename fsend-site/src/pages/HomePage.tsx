import { createSignal, For, Show, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiUpload, FiDownload } from "solid-icons/fi";
import { handleDrop } from "../lib/files/source";
import { setPendingDrop } from "../lib/pendingDrop";
import { hasFileSystemAccess } from "../lib/files/storage";
import { GITHUB_URL, MDN_FS_API, MDN_WEBRTC, SITE_URL } from "../lib/links";
import { createWindowDropTarget } from "../primitives/createWindowDropTarget";
import { Logo } from "../components/Logo";

function Ext(props: { href: string; children: JSX.Element }) {
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

const STEPS = [
  {
    n: "01",
    accent: "text-flame",
    title: "Pick your files",
    body: (
      <>
        Select individual files and folders or drag and drop them anywhere on
        this page
      </>
    ),
  },
  {
    n: "02",
    accent: "text-flame",
    title: "Share the code",
    body: (
      <>
        You get a one-time code and a QR link. Share either with the receiver.
      </>
    ),
  },
  {
    n: "03",
    accent: "text-azure",
    title: "Direct transfer",
    body: (
      <>
        Both browsers connect over <Ext href={MDN_WEBRTC}>WebRTC</Ext> and the
        files stream directly between them.
      </>
    ),
  },
];

const FAQ_ITEMS = [
  {
    q: "Is there a file size limit?",
    a: (
      <>
        No — files stream straight from one device to the other, so you're
        limited only by your connection and the receiver's free disk space.
      </>
    ),
  },
  {
    q: "Do both sides need to be online?",
    a: (
      <>
        Yes — nothing is stored in between, so both tabs have to stay open until
        the transfer finishes.
      </>
    ),
  },
  {
    q: "Can fsend see my files?",
    a: (
      <>
        No — fsend uses end to end encrypted <Ext href={MDN_WEBRTC}>WebRTC</Ext>{" "}
        data channels. File data never touches our servers.
      </>
    ),
  },
  {
    q: "Which browsers work best?",
    a: (
      <>
        Anything with WebRTC. Chrome, Edge and Opera also ship the{" "}
        <Ext href={MDN_FS_API}>File System Access API</Ext>, so fsend writes
        straight to disk and can resume interrupted transfers. Firefox and
        Safari buffer the transfer in memory and save a zip at the end, which
        caps a transfer at your available RAM.
      </>
    ),
  },
  {
    q: "Can I use it outside the browser?",
    a: (
      <>
        Yes — fsend is available as a command-line tool. You can find the
        installation instructions on <Ext href={GITHUB_URL}>GitHub</Ext>.
      </>
    ),
  },
  {
    q: "Is it free?",
    a: (
      <>
        Yes — free and open source. The code is on{" "}
        <Ext href={GITHUB_URL}>GitHub</Ext>.
      </>
    ),
  },
];

function Eyebrow(props: { children: string }) {
  return (
    <div class="font-mono text-xs tracking-[0.14em] uppercase text-ink-dim">
      {props.children}
    </div>
  );
}

export function HomePage() {
  const navigate = useNavigate();
  const canUseDisk = hasFileSystemAccess();
  const [isProcessing, setIsProcessing] = createSignal(false);

  const isDragging = createWindowDropTarget(async (data) => {
    setIsProcessing(true);
    try {
      const entries = await handleDrop(data);
      if (entries.length > 0) {
        setPendingDrop(entries);
        navigate("/send");
      }
    } finally {
      setIsProcessing(false);
    }
  });

  return (
    <div class="flex-1">
      <Title>fsend — Free P2P File Transfer in Your Browser</Title>
      <Meta
        name="description"
        content="Send files of any size directly between devices, peer-to-peer over WebRTC. End-to-end encrypted, no uploads, no accounts, no size limits. Free and open source."
      />
      <Link rel="canonical" href={`${SITE_URL}/`} />

      <Show when={isProcessing()}>
        <div class="fixed inset-0 z-50 bg-canvas/85 flex items-center justify-center">
          <div class="flex flex-col items-center">
            <div class="animate-spin w-14 h-14 border-4 border-line border-t-azure rounded-full" />
            <p class="mt-4 text-ink-muted font-medium">Processing files...</p>
          </div>
        </div>
      </Show>

      <section class="fills-viewport relative px-5 sm:px-6 pt-10 pb-20 flex flex-col items-center justify-center text-center">
        <div class="flex items-center gap-2 sm:gap-3 mb-4">
          <Logo class="w-[62px] sm:w-[82px]" />
          <h1 class="text-5xl sm:text-6xl font-bold tracking-[-0.025em]">
            fsend
          </h1>
        </div>

        <p class="text-lg sm:text-[19px] text-ink-muted max-w-2xl text-pretty">
          Direct peer-to-peer transfers in your browser using WebRTC
        </p>

        <div class="flex flex-wrap items-center justify-center gap-x-2.5 gap-y-1 mt-4 font-mono text-[12.5px] text-ink-dim">
          <span>No filesize limits</span>
          <span class="text-ink-faint">·</span>
          <span>End-to-end encrypted</span>
          <span class="text-ink-faint">·</span>
          <span>Free</span>
          <span class="text-ink-faint">·</span>
          <span>No account</span>
        </div>

        <div class="w-full max-w-[560px] mt-10 flex flex-col gap-3.5">
          <Show when={!canUseDisk}>
            <div class="flex gap-3 items-start text-left bg-warn-bg border border-warn-line text-warn-ink rounded-lg px-4 py-3.5 text-sm leading-relaxed">
              <div>
                Your browser has limited support. Folders will be downloaded as
                zip files and transfer resumption won't be available when
                receiving.
              </div>
            </div>
          </Show>

          {/* The slab sits behind the face and shows through as it lifts. */}
          <A
            href="/send"
            draggable={false}
            class="theme-flame bg-accent-deep rounded-xl border-none cursor-pointer font-bold text-lg group select-none"
          >
            <span
              class={`block box-border border-2 border-accent-line rounded-xl py-3 px-6 bg-accent-soft text-accent-ink transition-all duration-150 text-center ${
                isDragging()
                  ? "bg-accent-soft-hi -translate-y-0.5"
                  : "group-hover:bg-accent-soft-hi group-hover:-translate-y-0.5"
              }`}
            >
              <FiUpload
                class="inline-block w-5 h-5 mr-2 -mt-1"
                aria-hidden="true"
              />
              {isDragging() ? "Drop to Send" : "Send Files"}
            </span>
          </A>

          <A
            href="/receive"
            draggable={false}
            class="theme-azure bg-accent-deep rounded-xl border-none cursor-pointer font-bold text-lg group select-none"
          >
            <span class="block box-border border-2 border-accent-line rounded-xl py-3 px-6 bg-accent-soft text-accent-ink transition-all duration-150 group-hover:bg-accent-soft-hi group-hover:-translate-y-0.5 text-center">
              <FiDownload
                class="inline-block w-5 h-5 mr-2 -mt-1"
                aria-hidden="true"
              />
              Receive Files
            </span>
          </A>

          <p class="text-sm text-ink-dim mt-1.5">
            Drop a file anywhere on this page to start
          </p>
        </div>

        <a
          href="#how"
          aria-label="Scroll to how it works"
          class="absolute bottom-4 left-1/2 -translate-x-1/2 p-2 text-ink-dim hover:text-ink transition-colors"
        >
          <svg
            viewBox="0 0 28 9"
            class="scroll-hint w-10"
            fill="none"
            stroke="currentColor"
            stroke-width="1.75"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M1.5 1.5 L14 7.5 L26.5 1.5" />
          </svg>
        </a>
      </section>

      <section
        id="how"
        class="border-t border-line-soft px-5 sm:px-10 py-16 sm:py-18 flex flex-col items-center gap-10"
      >
        <div class="flex flex-col items-center gap-2 text-center">
          <Eyebrow>How it works</Eyebrow>
        </div>

        <ol class="grid sm:grid-cols-3 gap-4.5 max-w-[1000px] w-full">
          <For each={STEPS}>
            {(step) => (
              <li class="bg-surface border border-line rounded-xl p-6 flex flex-col gap-2.5">
                <div class={`font-mono text-[13px] ${step.accent}`}>
                  {step.n}
                </div>
                <h3 class="font-bold text-lg">{step.title}</h3>
                <p class="text-ink-muted text-[15px] leading-relaxed">
                  {step.body}
                </p>
              </li>
            )}
          </For>
        </ol>
      </section>

      <section
        id="faq"
        class="border-t border-line-soft px-5 sm:px-10 py-16 sm:py-18 flex flex-col items-center gap-9"
      >
        <div class="flex flex-col items-center gap-2 text-center">
          <Eyebrow>FAQ</Eyebrow>
        </div>

        <div class="grid md:grid-cols-2 gap-4.5 max-w-[1000px] w-full">
          <For each={FAQ_ITEMS}>
            {(item) => (
              <div class="bg-surface border border-line rounded-xl p-6 flex flex-col gap-2">
                <h3 class="font-bold text-[17px]">{item.q}</h3>
                <p class="text-ink-muted text-[15px] leading-relaxed">
                  {item.a}
                </p>
              </div>
            )}
          </For>
        </div>
      </section>
    </div>
  );
}
