import ReactMarkdown from "react-markdown";
import rehypeSanitize from "rehype-sanitize";

/**
 * Rendered view of the canonical report markdown.
 *
 * The markdown contains model-generated text, so raw HTML inside it is
 * never turned into markup. Raw HTML is skipped, the resulting HAST is
 * passed through an explicit allowlist sanitizer, and images are disabled
 * so generated Markdown cannot trigger a passive request to a tracking URL.
 * The canonical bytes themselves are not touched here — they stay available
 * verbatim through the Download Markdown and Download PDF actions.
 */
export function ReportMarkdown({ markdown }: { markdown: string }) {
  return (
    <article className="report-body">
      <ReactMarkdown
        skipHtml
        rehypePlugins={[rehypeSanitize]}
        disallowedElements={["img"]}
        components={{
          // `node` is react-markdown's HAST node, not a DOM attribute;
          // it is destructured away so it never reaches the element.
          a: ({ node, children, ...props }) => (
            <a {...props} rel="noreferrer noopener">
              {children}
            </a>
          ),
        }}
      >
        {markdown}
      </ReactMarkdown>
    </article>
  );
}
