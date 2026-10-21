// A synthetic TSX sample.
type Props = { title: string; count: number };

export function Badge({ title, count }: Props) {
  const shown = count > 99 ? "99+" : String(count);
  return (
    <div className="badge" aria-label={title}>
      <Icon name="dot" />
      {shown}
    </div>
  );
}
