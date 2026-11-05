export function LoadingSpinner({ message = 'Loading...' }: { message?: string }) {
  return (
    <div role="status" className="flex items-center justify-center py-12">
      <div aria-hidden="true" className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent mr-3" />
      <span className="text-gray-400">{message}</span>
    </div>
  );
}
