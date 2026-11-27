import { common, Want } from '@kit.AbilityKit'

export interface AppLauncher {
  id: string
  ability: string
  labelResource: string
  iconResource: string
  label: Resource
  icon: Resource
}

export class AppLauncherRegistry {
  private readonly apps: Map<string, AppLauncher> = new Map()

  register(app: AppLauncher): void {
    if (!app.id || this.apps.has(app.id)) {
      throw new Error('应用入口 ID 为空或已注册：' + app.id)
    }
    this.apps.set(app.id, app)
  }

  ids(): string[] {
    return Array.from(this.apps.keys())
  }

  get(id: string): AppLauncher {
    const app = this.apps.get(id)
    if (!app) throw new Error('应用入口未注册：' + id)
    return app
  }
}

export function appWant(context: common.UIAbilityContext, app: AppLauncher): Want {
  return { bundleName: context.abilityInfo.bundleName, moduleName: context.abilityInfo.moduleName,
    abilityName: app.ability }
}
