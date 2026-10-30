// The pt-BR catalog: one file per area of the app (each area owns its keys, prefixed with its name).
import common from "./pt-BR/common.json";
import files from "./pt-BR/files.json";
import menu from "./pt-BR/menu.json";
import views from "./pt-BR/views.json";
import kinds from "./pt-BR/kinds.json";
import organize from "./pt-BR/organize.json";
import format from "./pt-BR/format.json";
import fileops from "./pt-BR/fileops.json";
import names from "./pt-BR/names.json";
import window from "./pt-BR/window.json";
import sidebar from "./pt-BR/sidebar.json";
import toolbar from "./pt-BR/toolbar.json";
import tasks from "./pt-BR/tasks.json";
import properties from "./pt-BR/properties.json";
import map from "./pt-BR/map.json";
import settings from "./pt-BR/settings.json";
import about from "./pt-BR/about.json";
import terminal from "./pt-BR/terminal.json";
import tips from "./pt-BR/tips.json";
import dnd from "./pt-BR/dnd.json";
import type { Catalog } from "../index";

const catalog: Catalog = {
  ...common,
  ...files,
  ...menu,
  ...views,
  ...kinds,
  ...organize,
  ...format,
  ...fileops,
  ...names,
  ...window,
  ...sidebar,
  ...toolbar,
  ...tasks,
  ...properties,
  ...map,
  ...settings,
  ...about,
  ...terminal,
  ...tips,
  ...dnd,
};
export default catalog;
