import { importProgressView, type ProgressInput } from '../importProgress';

export default function ImportProgressMeter({ status, name }: { status: ProgressInput; name?: string }) {
  const view = importProgressView(status);
  return <div className="mb-3">
    <progress max={100} value={view.percent} aria-label={`SVN import progress${name ? ` — ${name}` : ''}`}
      aria-valuetext={view.description} className="w-full h-2 accent-blue-400" />
    <p className="sr-only" role="status" aria-atomic="true">{view.announcement}</p>
  </div>;
}
