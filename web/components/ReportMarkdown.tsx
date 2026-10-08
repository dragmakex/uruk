import ReactMarkdown from "react-markdown";

/**
 * Rendered view of the canonical report markdown.
 *
 * The markdown contains model-generated text, so raw HTML inside it is
 * never turned into markup: react-markdown without a raw-HTML plugin
 * drops HTML nodes entirely. The canonical bytes themselves are not
 * touched here — they stay available verbatim through the Download
 * Markdown and Download PDF actions next to this view.
 */
export function ReportMarkdown({ markdown }: { markdown: string }) {
  return (
    <article className="report-body">
      <ReactMarkdown>{markdown}</ReactMarkdown>
    </article>
  );
}
