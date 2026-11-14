import type { NavigationId } from "../types";

export type AppCopy = {
  navigation: Record<NavigationId, string>;
  settings: {
    about: string;
    aboutDescription: string;
    appearance: string;
    bodyRetention: string;
    bodyRetentionDescription: string;
    callLogs: string;
    callLogsDescription: string;
    captureBodies: string;
    captureBodiesDescription: string;
    close: string;
    data: string;
    done: string;
    general: string;
    language: string;
    languageChinese: string;
    languageEnglish: string;
    languageSystem: string;
    launchAtLogin: string;
    launchAtLoginDescription: string;
    observability: string;
    proxy: string;
    theme: string;
    themeDark: string;
    themeLight: string;
    themeSystem: string;
    title: string;
  };
  sidebar: {
    collapse: string;
    expand: string;
    primaryNavigation: string;
  };
  text: Record<string, string>;
};
