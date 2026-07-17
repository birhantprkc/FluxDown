// FluxCloud 云账户 —— Wire 契约 v1（camelCase，见 FluxCloud/server），与本地下载器
// api.ts 的 types.ts 完全独立，互不引用。

/** 用户状态：0=active 1=disabled 2=pending（待邮箱验证）。 */
export type CloudUserStatus = 'active' | 'disabled' | 'pending'

export interface CloudUser {
  id: string
  email: string
  nickname: string
  plan: string
  status: CloudUserStatus
  /** Origin ID(v1.2 新增):类 QQ 号唯一数字身份,从 10001 起严格递增;pending 用户为 null。 */
  originId: number | null
  createdAt: string
  lastLoginAt?: string
}

/** 套餐能力集：服务端自由演进字段，本文件只按需声明已知字段，未知字段仍可原样读取。 */
export interface Entitlements {
  maxSyncDevices?: number
  [key: string]: unknown
}

/** 受信任设备（DeviceDto，v1.1 增补 lastIp/appVersion，均可空）。 */
export interface CloudDevice {
  id: string
  deviceId: string
  name: string
  platform?: string
  /** 最近登录 IP，服务端按 X-Forwarded-For/X-Real-IP 记录，可能为空。 */
  lastIp?: string
  /** 客户端版本号，登录/信任设备时上报，可能为空（如旧版客户端未上报）。 */
  appVersion?: string
  createdAt: string
  lastSeenAt: string
}

/** 登录/注册验证/验证码登录 成功后的统一响应。 */
export interface AuthResponse {
  accessToken: string
  refreshToken: string
  expiresIn: number
  user: CloudUser
  entitlements: Entitlements
  device: CloudDevice
}

/** POST /auth/login 的 tagged 响应：设备已受信任直接下发令牌，新设备则要求邮箱验证码。 */
export type LoginResult =
  | { status: 'ok'; auth: AuthResponse }
  | { status: 'deviceVerificationRequired'; ttlSeconds: number }

/** GET /me 响应：UserDto 字段打平 + entitlements。 */
export interface CloudProfile extends CloudUser {
  entitlements: Entitlements
}

/** POST /auth/register、/auth/code/send 等发码接口的响应。 */
export interface TtlResponse {
  ttlSeconds: number
}

/** GET /devices 响应。 */
export interface DevicesResponse {
  devices: CloudDevice[]
}

/** 服务端错误统一形态 `{code, message}`，附带 HTTP 状态码方便按 code/status 分支处理。 */
export class CloudApiError extends Error {
  code: string
  status: number
  constructor(code: string, message: string, status: number) {
    super(message)
    this.code = code
    this.status = status
  }
}
