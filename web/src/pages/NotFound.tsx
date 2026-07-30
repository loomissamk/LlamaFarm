import { ArrowLeft } from 'lucide-react';
import { Link } from 'react-router-dom';
import { useLocale } from '@/lib/i18n';

export default function NotFound() {
  const { t } = useLocale();

  return (
    <section
      className="mx-auto flex min-h-[calc(100dvh-3.5rem)] max-w-2xl flex-col items-center justify-center px-5 py-12 text-center"
      aria-labelledby="not-found-title"
    >
      <p className="text-sm font-semibold uppercase tracking-[0.24em] text-blue-300">
        404
      </p>
      <h2 id="not-found-title" className="mt-3 text-3xl font-semibold text-white">
        {t('not_found.title')}
      </h2>
      <p className="mt-3 max-w-lg text-sm leading-6 text-gray-400">
        {t('not_found.description')}
      </p>
      <Link
        to="/"
        className="mt-7 inline-flex min-h-11 items-center gap-2 rounded-lg bg-blue-600 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-blue-500"
      >
        <ArrowLeft className="h-4 w-4" aria-hidden="true" />
        {t('not_found.home')}
      </Link>
    </section>
  );
}
