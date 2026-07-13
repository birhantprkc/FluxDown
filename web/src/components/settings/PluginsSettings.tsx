// 插件管理：已安装插件列表（启用开关 + 设置表单 + 卸载，disabledReason 徽标区分手动/熔断）
// + 安装区（zip 文件上传 / dev 模式目录路径引用）。

import { type ChangeEvent, type ReactNode, useRef, useState } from 'react'
import { Check, Download, Link2, Trash2, Upload } from 'lucide-react'
import { cn } from '../../lib/cn'
import { confirmDialog } from '../../lib/confirm'
import { type I18nKey, translateBackendMessage, useI18n } from '../../lib/i18n'
import type { MarketEntry, PluginDto } from '../../lib/types'
import {
  useInstallFromMarket,
  useInstallPluginDevMutation,
  useInstallPluginMutation,
  useMarketQuery,
  usePluginsQuery,
  useSetPluginEnabledMutation,
  useUninstallPluginMutation,
  useUpdatePluginSettingsMutation,
} from '../../hooks/usePlugins'
import { SetRow, SetSwitch } from './controls'
import { PluginSettingForm } from './PluginSettingForm'

export function PluginsSettings() {
  const { t } = useI18n()
  const { data: plugins, isLoading, isError } = usePluginsQuery()
  const installMut = useInstallPluginMutation()
  const installDevMut = useInstallPluginDevMutation()
  const [devPath, setDevPath] = useState('')
  const fileRef = useRef<HTMLInputElement>(null)
  const { data: market, isLoading: marketLoading, isError: marketError } = useMarketQuery()
  const installFromMarketMut = useInstallFromMarket()
  const installedIds = new Set(plugins?.map((p) => p.identity) ?? [])

  function onZipChosen(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    e.target.value = ''
    if (file) installMut.mutate(file)
  }

  function installDev() {
    const dir = devPath.trim()
    if (!dir) return
    installDevMut.mutate(dir, { onSuccess: () => setDevPath('') })
  }

  const installError = installMut.error ?? installDevMut.error

  return (
    <div className="max-w-[640px]">
      <h2 className="set-title">{t('set.plugins')}</h2>
      <p className="set-desc">{t('set.plugins.desc')}</p>

      <div className="set-group">
        <SetRow title={t('plugins.installZip')} desc={t('plugins.installZipDesc')}>
          <input ref={fileRef} type="file" accept=".zip" className="hidden" onChange={onZipChosen} />
          <button
            type="button"
            className="btn ghost sm flex-shrink-0"
            onClick={() => fileRef.current?.click()}
            disabled={installMut.isPending}
          >
            <Upload size={14} />
            {installMut.isPending ? t('common.loading') : t('plugins.installZip')}
          </button>
        </SetRow>
        <SetRow title={t('plugins.installDev')} desc={t('plugins.installDevDesc')}>
          <div className="flex flex-shrink-0 items-center gap-2" style={{ width: 300 }}>
            <input
              className="text-input flex-1"
              placeholder={t('plugins.devPathPlaceholder')}
              value={devPath}
              onChange={(e) => setDevPath(e.target.value)}
            />
            <button
              type="button"
              className="btn ghost sm flex-shrink-0"
              onClick={installDev}
              disabled={installDevMut.isPending || devPath.trim() === ''}
            >
              {installDevMut.isPending ? t('common.loading') : t('plugins.installDev')}
            </button>
          </div>
        </SetRow>
        {installError && (
          <p className="px-4 pb-3 text-[12px] text-danger">
            {t('plugins.installFailed', { error: translateBackendMessage(installError.message) })}
          </p>
        )}
      </div>

      {isLoading ? (
        <p className="set-desc">{t('common.loading')}</p>
      ) : isError ? (
        <p className="set-desc text-danger">{t('set.loadFailed')}</p>
      ) : !plugins || plugins.length === 0 ? (
        <p className="set-desc">{t('plugins.empty')}</p>
      ) : (
        <div className="flex flex-col gap-3">
          {plugins.map((p) => (
            <PluginCard key={p.identity} plugin={p} />
          ))}
        </div>
      )}

      <h2 className="set-title mt-7">{t('market.title')}</h2>
      <p className="set-desc">{t('market.desc')}</p>
      {marketLoading ? (
        <p className="set-desc">{t('common.loading')}</p>
      ) : marketError ? (
        <p className="set-desc text-danger">{t('market.loadFailed')}</p>
      ) : !market || market.length === 0 ? (
        <p className="set-desc">{t('market.empty')}</p>
      ) : (
        <div className="flex flex-col gap-3">
          {market.map((entry) => (
            <MarketCard
              key={entry.pluginId}
              entry={entry}
              installed={installedIds.has(entry.pluginId)}
              installMut={installFromMarketMut}
            />
          ))}
        </div>
      )}
    </div>
  )
}

type BadgeTone = 'accent' | 'neutral' | 'danger'

function Badge({ tone, children }: { tone: BadgeTone; children: ReactNode }) {
  return (
    <span
      className={cn(
        'rounded-full px-2 py-0.5 text-[11px] font-medium',
        tone === 'accent' && 'bg-accent-weak text-accent',
        tone === 'neutral' && 'bg-surface2 text-text3',
        tone === 'danger' && 'bg-danger/10 text-danger',
      )}
    >
      {children}
    </span>
  )
}

function DisabledBadge({ reason }: { reason: PluginDto['disabledReason'] }) {
  const { t } = useI18n()
  if (reason === 'None') return null
  const manual = reason === 'Manual'
  return <Badge tone={manual ? 'neutral' : 'danger'}>{manual ? t('plugins.disabledManual') : t('plugins.disabledCircuitBreaker')}</Badge>
}

function PluginCard({ plugin }: { plugin: PluginDto }) {
  const { t } = useI18n()
  const enabledMut = useSetPluginEnabledMutation()
  const settingsMut = useUpdatePluginSettingsMutation()
  const uninstallMut = useUninstallPluginMutation()

  async function uninstall() {
    const ok = await confirmDialog({
      title: t('plugins.uninstallTitle'),
      message: t('plugins.uninstallMsg', { name: plugin.name }),
      danger: true,
    })
    if (ok) uninstallMut.mutate(plugin.identity)
  }

  return (
    <div className="rounded-xl border border-line bg-surface p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <b className="text-[13px] font-semibold">{plugin.name}</b>
            <span className="text-[11px] tabular-nums text-text3">v{plugin.version}</span>
            {plugin.devMode && <Badge tone="accent">{t('plugins.devMode')}</Badge>}
            <DisabledBadge reason={plugin.disabledReason} />
          </div>
          {plugin.description && <p className="mt-1.5 text-[12px] leading-relaxed text-text2">{plugin.description}</p>}
          {plugin.homepage && (
            <a
              className="mt-1.5 inline-flex items-center gap-1 text-[11.5px] text-accent hover:underline"
              href={plugin.homepage}
              target="_blank"
              rel="noreferrer"
            >
              <Link2 size={11} />
              {plugin.homepage}
            </a>
          )}
        </div>
        <div className="flex flex-shrink-0 items-center gap-2">
          <SetSwitch
            checked={plugin.enabled}
            onCheckedChange={(v) => enabledMut.mutate({ identity: plugin.identity, enabled: v })}
          />
          <button
            type="button"
            className="icon-btn sm text-text3 hover:text-danger"
            title={t('plugins.uninstallTitle')}
            aria-label={t('plugins.uninstallTitle')}
            onClick={() => void uninstall()}
            disabled={uninstallMut.isPending}
          >
            <Trash2 size={14} />
          </button>
        </div>
      </div>
      {plugin.settings.length > 0 && (
        <div className="mt-3 border-t border-line pt-3">
          <PluginSettingForm
            plugin={plugin}
            saving={settingsMut.isPending}
            onSave={(entries) => settingsMut.mutate({ identity: plugin.identity, entries })}
          />
        </div>
      )}
    </div>
  )
}

const YANKED_KEYS: Record<string, I18nKey> = {
  deprecated: 'market.yanked.deprecated',
  vulnerable: 'market.yanked.vulnerable',
  malicious: 'market.yanked.malicious',
}

function YankedBadge({ yanked }: { yanked: string }) {
  const { t } = useI18n()
  if (yanked === '' || yanked === 'none') return null
  const key = YANKED_KEYS[yanked]
  return <Badge tone="danger">{key ? t(key) : yanked}</Badge>
}

function MarketCard({
  entry,
  installed,
  installMut,
}: {
  entry: MarketEntry
  installed: boolean
  installMut: ReturnType<typeof useInstallFromMarket>
}) {
  const { t } = useI18n()
  const pending = installMut.isPending && installMut.variables === entry.pluginId
  const label = entry.name || entry.pluginId
  const initial = label.trim().charAt(0).toUpperCase() || '?'

  return (
    <div className="rounded-xl border border-line bg-surface p-4">
      <div className="flex items-start gap-3">
        <div className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-lg bg-accent-weak text-[13px] font-semibold text-accent">
          {initial}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <b className="text-[13px] font-semibold">{label}</b>
            <span className="text-[11px] tabular-nums text-text3">v{entry.version}</span>
            {entry.author && (
              <span className="text-[11px] text-text3">
                <span className="opacity-50">· </span>
                {entry.author}
              </span>
            )}
            <YankedBadge yanked={entry.yanked} />
          </div>
          {entry.description && <p className="mt-1.5 text-[12px] leading-relaxed text-text2">{entry.description}</p>}
          {entry.tags.length > 0 && (
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {entry.tags.map((tag) => (
                <Badge key={tag} tone="neutral">
                  {tag}
                </Badge>
              ))}
            </div>
          )}
          {entry.homepage && (
            <a
              className="mt-1.5 inline-flex items-center gap-1 text-[11.5px] text-accent hover:underline"
              href={entry.homepage}
              target="_blank"
              rel="noreferrer"
            >
              <Link2 size={11} />
              {entry.homepage}
            </a>
          )}
          {installMut.isError && installMut.variables === entry.pluginId && (
            <p className="mt-1.5 text-[12px] text-danger">
              {t('market.installFailed', { error: translateBackendMessage(installMut.error.message) })}
            </p>
          )}
        </div>
        <button
          type="button"
          className={cn('btn sm flex-shrink-0', installed ? 'ghost' : 'primary')}
          onClick={() => installMut.mutate(entry.pluginId)}
          disabled={installed || pending}
        >
          {installed ? (
            <>
              <Check size={14} />
              {t('market.installed')}
            </>
          ) : pending ? (
            t('common.loading')
          ) : (
            <>
              <Download size={14} />
              {t('market.install')}
            </>
          )}
        </button>
      </div>
    </div>
  )
}
