import { A } from "@solidjs/router";
import { Title, Meta, Link } from "@solidjs/meta";
import { FiArrowLeft } from "solid-icons/fi";

const FAQ_ITEMS = [
  {
    q: "Is fsend free?",
    a: "Yes. fsend is completely free and open source. You can view the code and contribute on GitHub.",
  },
  {
    q: "Is my file uploaded to a server?",
    a: "No. Transfers are peer-to-peer over WebRTC. A rendezvous server only helps the two clients discover each other; your file bytes never pass through it.",
  },
  {
    q: "Is there a file size limit?",
    a: "No hard limit. Transfers stream directly between devices, so you're bounded only by your connection and available disk space.",
  },
  {
    q: "Is the transfer encrypted?",
    a: "Yes. WebRTC uses DTLS-SRTP end-to-end encryption by default, so data in transit is encrypted between the two browsers.",
  },
  {
    q: "Is there a CLI version?",
    a: "Yes. Install fsend-cli with `cargo install fsend-cli`. It uses the same peer-to-peer approach and works entirely from the terminal.",
  },
];

const FAQ_JSONLD = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: FAQ_ITEMS.map((item) => ({
    "@type": "Question",
    name: item.q,
    acceptedAnswer: {
      "@type": "Answer",
      text: item.a,
    },
  })),
};

export function AboutPage() {
  return (
    <div class="flex-1 bg-indigo-100 dark:bg-neutral-900 py-8 px-4 transition-colors">
      <Title>About fsend — How Peer-to-Peer File Transfer Works</Title>
      <Meta
        name="description"
        content="Learn how fsend works: browser-to-browser file transfers over WebRTC, end-to-end encrypted, with no server upload and no file size limits. Answers to common questions."
      />
      <Link rel="canonical" href="https://fsend.sh/about" />

      <script type="application/ld+json" innerHTML={JSON.stringify(FAQ_JSONLD)} />

      <div class="max-w-3xl mx-auto">
        <A
          href="/"
          class="mb-6 text-blue-600 dark:text-blue-400 hover:text-blue-800 dark:hover:text-blue-300 flex items-center gap-2 transition-colors"
        >
          <FiArrowLeft class="w-4 h-4" aria-hidden="true" /> Back
        </A>

        <h1 class="text-3xl font-bold text-gray-800 dark:text-gray-100 mb-4">
          About fsend
        </h1>
        <p class="text-gray-700 dark:text-gray-300 mb-10">
          fsend is a peer-to-peer file transfer app that runs entirely in your
          browser. Files stream directly between the sender and receiver over
          an encrypted WebRTC connection — never uploaded to a server, never
          stored, no accounts required.
        </p>

        <section aria-labelledby="how-it-works" class="mb-12">
          <h2
            id="how-it-works"
            class="text-2xl font-bold text-gray-800 dark:text-gray-100 mb-6"
          >
            How fsend works
          </h2>
          <ol class="grid gap-4 sm:grid-cols-3">
            <li class="bg-white/60 dark:bg-neutral-800/60 border border-indigo-200 dark:border-neutral-700 rounded-xl p-5">
              <div class="text-sm font-semibold text-indigo-600 dark:text-indigo-400 mb-2">
                Step 1
              </div>
              <h3 class="font-semibold text-gray-800 dark:text-gray-100 mb-1">
                Pick your files
              </h3>
              <p class="text-sm text-gray-600 dark:text-gray-400">
                Drag and drop, or select files and folders. fsend generates a
                short session code.
              </p>
            </li>
            <li class="bg-white/60 dark:bg-neutral-800/60 border border-indigo-200 dark:border-neutral-700 rounded-xl p-5">
              <div class="text-sm font-semibold text-indigo-600 dark:text-indigo-400 mb-2">
                Step 2
              </div>
              <h3 class="font-semibold text-gray-800 dark:text-gray-100 mb-1">
                Share the code
              </h3>
              <p class="text-sm text-gray-600 dark:text-gray-400">
                Send the code (or the QR link) to the receiver via any channel
                — chat, email, or in person.
              </p>
            </li>
            <li class="bg-white/60 dark:bg-neutral-800/60 border border-indigo-200 dark:border-neutral-700 rounded-xl p-5">
              <div class="text-sm font-semibold text-indigo-600 dark:text-indigo-400 mb-2">
                Step 3
              </div>
              <h3 class="font-semibold text-gray-800 dark:text-gray-100 mb-1">
                Direct transfer
              </h3>
              <p class="text-sm text-gray-600 dark:text-gray-400">
                The receiver enters the code and the browsers connect
                peer-to-peer over WebRTC. Files stream directly, encrypted
                end-to-end.
              </p>
            </li>
          </ol>
        </section>

        <section aria-labelledby="faq" class="pb-8">
          <h2
            id="faq"
            class="text-2xl font-bold text-gray-800 dark:text-gray-100 mb-6"
          >
            Frequently asked questions
          </h2>
          <div class="space-y-3">
            {FAQ_ITEMS.map((item) => (
              <details class="group bg-white/60 dark:bg-neutral-800/60 border border-indigo-200 dark:border-neutral-700 rounded-xl">
                <summary class="cursor-pointer list-none px-5 py-4 font-semibold text-gray-800 dark:text-gray-100 flex items-center justify-between">
                  <span>{item.q}</span>
                  <span
                    class="ml-4 text-indigo-500 group-open:rotate-45 transition-transform"
                    aria-hidden="true"
                  >
                    +
                  </span>
                </summary>
                <div class="px-5 pb-4 text-sm text-gray-600 dark:text-gray-400">
                  {item.a}
                </div>
              </details>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
