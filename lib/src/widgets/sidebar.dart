import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../services/update_service.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';

class Sidebar extends StatefulWidget {
  final DownloadController controller;

  const Sidebar({super.key, required this.controller});

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  /// 分类区块是否展开（默认折叠，降低视觉噪音）
  bool _categoryExpanded = false;

  // ─────────────────────────────────────────────
  // 图标映射
  // ─────────────────────────────────────────────

  static IconData _statusIcon(StatusTab tab) => switch (tab) {
    StatusTab.all => LucideIcons.layoutGrid,
    StatusTab.downloading => LucideIcons.download,
    StatusTab.completed => LucideIcons.circleCheck,
    StatusTab.paused => LucideIcons.circlePause,
    StatusTab.error => LucideIcons.circleAlert,
  };

  static IconData _categoryIcon(FileCategory cat) => switch (cat) {
    FileCategory.all => LucideIcons.folders,
    FileCategory.video => LucideIcons.film,
    FileCategory.audio => LucideIcons.music,
    FileCategory.document => LucideIcons.fileText,
    FileCategory.image => LucideIcons.image,
    FileCategory.archive => LucideIcons.archive,
    FileCategory.other => LucideIcons.file,
  };

  static String _statusLabel(S s, StatusTab tab) => switch (tab) {
    StatusTab.all => s.tabAll,
    StatusTab.downloading => s.tabDownloading,
    StatusTab.completed => s.tabCompleted,
    StatusTab.paused => s.tabPaused,
    StatusTab.error => s.tabError,
  };

  // ─────────────────────────────────────────────
  // Build
  // ─────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ListenableBuilder(
      listenable: widget.controller,
      builder: (context, _) {
        final ctrl = widget.controller;
        final s = LocaleScope.of(context);
        return Container(
          width: 224,
          color: c.surface1,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _buildLogo(c),
              const SizedBox(height: 10),
              _buildStatusSection(ctrl, s, c),
              const SizedBox(height: 6),
              _buildCategorySection(ctrl, s, c),
              const Spacer(),
              const _UpdateFooter(),
            ],
          ),
        );
      },
    );
  }

  // ─────────────────────────────────────────────
  // Logo
  // ─────────────────────────────────────────────

  Widget _buildLogo(AppColors c) {
    return DragToMoveArea(
      child: Container(
        height: 48,
        padding: const EdgeInsets.symmetric(horizontal: 16),
        alignment: Alignment.centerLeft,
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            ClipRRect(
              borderRadius: BorderRadius.circular(5),
              child: Image.asset(
                'assets/logo/fluxdown_logo.png',
                width: 22,
                height: 22,
                filterQuality: FilterQuality.medium,
              ),
            ),
            const SizedBox(width: 9),
            Text.rich(
              TextSpan(
                children: [
                  TextSpan(
                    text: 'Flux',
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: c.accent,
                      letterSpacing: 0.3,
                    ),
                  ),
                  TextSpan(
                    text: 'Down',
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w500,
                      color: c.textPrimary,
                      letterSpacing: 0.3,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  // ─────────────────────────────────────────────
  // 状态区块（主导航）
  // ─────────────────────────────────────────────

  Widget _buildStatusSection(DownloadController ctrl, S s, AppColors c) {
    final selectedStatus = ctrl.statusTab;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _SectionHeader(title: s.sidebarStatus, c: c),
        const SizedBox(height: 4),
        for (final tab in StatusTab.values)
          _NavItem(
            icon: _statusIcon(tab),
            label: _statusLabel(s, tab),
            count: ctrl.countForStatus(tab),
            isSelected: selectedStatus == tab,
            showActivityDot:
                tab == StatusTab.downloading && ctrl.downloadingCount > 0,
            onTap: () => ctrl.setStatusTab(tab),
          ),
      ],
    );
  }

  // ─────────────────────────────────────────────
  // 分类区块（可折叠）
  // ─────────────────────────────────────────────

  Widget _buildCategorySection(DownloadController ctrl, S s, AppColors c) {
    final selectedCategory = ctrl.categoryFilter;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _CollapsibleSectionHeader(
          title: s.sidebarCategory,
          expanded: _categoryExpanded,
          c: c,
          onToggle: () =>
              setState(() => _categoryExpanded = !_categoryExpanded),
        ),
        if (_categoryExpanded) ...[
          const SizedBox(height: 4),
          for (final cat in FileCategory.values)
            _NavItem(
              icon: _categoryIcon(cat),
              label: cat.label,
              count: ctrl.countForCategory(cat),
              isSelected: selectedCategory == cat,
              onTap: () => ctrl.setCategoryFilter(cat),
            ),
        ],
      ],
    );
  }
}

// =============================================================================
// Section Headers
// =============================================================================

class _SectionHeader extends StatelessWidget {
  final String title;
  final AppColors c;

  const _SectionHeader({required this.title, required this.c});

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
      child: Text(
        title,
        style: TextStyle(
          fontSize: 10.5,
          fontWeight: FontWeight.w500,
          color: c.textMuted,
          letterSpacing: 0.5,
        ),
      ),
    );
  }
}

class _CollapsibleSectionHeader extends StatefulWidget {
  final String title;
  final bool expanded;
  final AppColors c;
  final VoidCallback onToggle;

  const _CollapsibleSectionHeader({
    required this.title,
    required this.expanded,
    required this.c,
    required this.onToggle,
  });

  @override
  State<_CollapsibleSectionHeader> createState() =>
      _CollapsibleSectionHeaderState();
}

class _CollapsibleSectionHeaderState
    extends State<_CollapsibleSectionHeader> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onToggle,
        child: Container(
          color: Colors.transparent,
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 5),
          child: Row(
            children: [
              Text(
                widget.title,
                style: TextStyle(
                  fontSize: 10.5,
                  fontWeight: FontWeight.w500,
                  color: _isHovered ? c.textSecondary : c.textMuted,
                  letterSpacing: 0.5,
                ),
              ),
              const Spacer(),
              Icon(
                widget.expanded
                    ? LucideIcons.chevronDown
                    : LucideIcons.chevronRight,
                size: 11,
                color: _isHovered ? c.textSecondary : c.textMuted,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// Nav Item
// =============================================================================

class _NavItem extends StatefulWidget {
  final IconData icon;
  final String label;
  final int? count;
  final bool isSelected;
  final bool showActivityDot;
  final VoidCallback onTap;

  const _NavItem({
    required this.icon,
    required this.label,
    this.count,
    required this.isSelected,
    this.showActivityDot = false,
    required this.onTap,
  });

  @override
  State<_NavItem> createState() => _NavItemState();
}

class _NavItemState extends State<_NavItem> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final selected = widget.isSelected;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          height: 32,
          margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
          padding: const EdgeInsets.symmetric(horizontal: 8),
          decoration: BoxDecoration(
            color: selected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : Colors.transparent,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            children: [
              // 活跃下载点 or 图标
              Stack(
                clipBehavior: Clip.none,
                children: [
                  Icon(
                    widget.icon,
                    size: 14,
                    color: selected ? c.accent : c.textSecondary,
                  ),
                  if (widget.showActivityDot)
                    Positioned(
                      top: -2,
                      right: -3,
                      child: Container(
                        width: 6,
                        height: 6,
                        decoration: BoxDecoration(
                          color: AppColors.green,
                          shape: BoxShape.circle,
                          border: Border.all(color: c.surface1, width: 1),
                        ),
                      ),
                    ),
                ],
              ),
              const SizedBox(width: 8),
              Text(
                widget.label,
                style: TextStyle(
                  fontSize: 12.5,
                  color: selected ? c.accent : c.textSecondary,
                  fontWeight: selected ? FontWeight.w500 : FontWeight.normal,
                ),
              ),
              if (widget.count != null) ...[
                const Spacer(),
                Text(
                  widget.count.toString(),
                  style: TextStyle(
                    fontSize: 11,
                    color: selected ? c.accent : c.textMuted,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// Sidebar footer: version display + update UI
// =============================================================================

class _UpdateFooter extends StatelessWidget {
  const _UpdateFooter();

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: UpdateService.instance,
      builder: (context, _) {
        final svc = UpdateService.instance;
        final c = AppColors.of(context);
        final status = svc.status;

        return Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (status == UpdateStatus.downloading) _buildProgressBar(svc, c),
            Container(
              height: 32,
              padding: const EdgeInsets.symmetric(horizontal: 12),
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.border, width: 1)),
              ),
              child: Row(
                children: [
                  Text(
                    _versionText(svc),
                    style: TextStyle(fontSize: 10.5, color: c.textMuted),
                  ),
                  const Spacer(),
                  _buildAction(context, svc, c, status),
                ],
              ),
            ),
          ],
        );
      },
    );
  }

  String _versionText(UpdateService svc) {
    final v = svc.currentVersion;
    final label = v == 'dev' ? 'dev' : 'v$v';
    if (svc.status == UpdateStatus.available ||
        svc.status == UpdateStatus.downloading ||
        svc.status == UpdateStatus.readyToInstall) {
      return '$label -> v${svc.checkResult?.latestVersion ?? ''}';
    }
    return label;
  }

  Widget _buildAction(
    BuildContext context,
    UpdateService svc,
    AppColors c,
    UpdateStatus status,
  ) {
    switch (status) {
      case UpdateStatus.available:
        return _UpdateActionButton(
          icon: LucideIcons.download,
          tooltip: LocaleScope.of(
            context,
          ).downloadUpdateVersion(svc.checkResult?.latestVersion ?? ''),
          color: AppColors.red,
          onTap: svc.downloadUpdate,
        );
      case UpdateStatus.downloading:
        final p = svc.progress;
        final pct = (p != null && p.totalBytes > 0)
            ? '${(p.downloadedBytes / p.totalBytes * 100).toStringAsFixed(0)}%'
            : '...';
        return Text(
          pct,
          style: TextStyle(
            fontSize: 10,
            color: c.accent,
            fontWeight: FontWeight.w600,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        );
      case UpdateStatus.readyToInstall:
        return _UpdateActionButton(
          icon: LucideIcons.rotateCcw,
          tooltip: LocaleScope.of(context).installAndRestart,
          color: AppColors.green,
          onTap: svc.installUpdate,
        );
      case UpdateStatus.checking:
        return SizedBox(
          width: 12,
          height: 12,
          child: CircularProgressIndicator(
            strokeWidth: 1.5,
            color: c.textMuted,
          ),
        );
      default:
        return const SizedBox.shrink();
    }
  }

  Widget _buildProgressBar(UpdateService svc, AppColors c) {
    final p = svc.progress;
    final fraction = (p != null && p.totalBytes > 0)
        ? (p.downloadedBytes / p.totalBytes).clamp(0.0, 1.0)
        : 0.0;

    return SizedBox(
      height: 3,
      child: LinearProgressIndicator(
        value: fraction,
        backgroundColor: c.surface2,
        valueColor: AlwaysStoppedAnimation<Color>(c.accent),
        minHeight: 3,
      ),
    );
  }
}

class _UpdateActionButton extends StatefulWidget {
  final IconData icon;
  final String tooltip;
  final Color color;
  final VoidCallback onTap;

  const _UpdateActionButton({
    required this.icon,
    required this.tooltip,
    required this.color,
    required this.onTap,
  });

  @override
  State<_UpdateActionButton> createState() => _UpdateActionButtonState();
}

class _UpdateActionButtonState extends State<_UpdateActionButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    return ShadTooltip(
      builder: (_) => Text(widget.tooltip),
      child: MouseRegion(
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: 22,
            height: 22,
            decoration: BoxDecoration(
              color: _isHovered
                  ? widget.color.withValues(alpha: 0.15)
                  : Colors.transparent,
              borderRadius: BorderRadius.circular(4),
            ),
            child: Icon(widget.icon, size: 13, color: widget.color),
          ),
        ),
      ),
    );
  }
}
