import { Fragment, type ReactNode } from "react";

/**
 * Fills `{name}` slots in a translated sentence with elements (code, ids), so
 * each language can put them where its word order needs them.
 */
export function rich(template: string, nodes: Record<string, ReactNode>): ReactNode {
  return template.split(/(\{\w+\})/).map((part, i) => {
    const name = /^\{(\w+)\}$/.exec(part)?.[1];
    return <Fragment key={i}>{name !== undefined && name in nodes ? nodes[name] : part}</Fragment>;
  });
}
