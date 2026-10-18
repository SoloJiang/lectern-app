import { useState } from "react";
import { Badge } from "./badge";

type PanelProps = {
  title: string;
  items: string[];
  onSelect?: (item: string) => void;
};

function Row({ label, onClick }: { label: string; onClick: () => void }) {
  return <li onClick={onClick}>{label}</li>;
}

export function Panel({ title, items, onSelect }: PanelProps) {
  const [open, setOpen] = useState(false);
  const toggle = () => setOpen(!open);
  return (
    <section>
      <h2 onClick={toggle}>
        {title} <Badge count={items.length} />
      </h2>
      {open && (
        <ul>
          {items.map((item) => (
            <Row key={item} label={item} onClick={() => onSelect?.(item)} />
          ))}
        </ul>
      )}
    </section>
  );
}

export default Panel;
