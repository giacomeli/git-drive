// The en catalog: one file per area of the app (each area owns its keys, prefixed with its name).
import common from "./en/common.json";
import files from "./en/files.json";
import menu from "./en/menu.json";
import views from "./en/views.json";
import kinds from "./en/kinds.json";
import organize from "./en/organize.json";
import format from "./en/format.json";
import fileops from "./en/fileops.json";
import names from "./en/names.json";
import window from "./en/window.json";
import sidebar from "./en/sidebar.json";
import toolbar from "./en/toolbar.json";
import tasks from "./en/tasks.json";
import properties from "./en/properties.json";
import map from "./en/map.json";
import settings from "./en/settings.json";
import about from "./en/about.json";
import terminal from "./en/terminal.json";
import tips from "./en/tips.json";
import dnd from "./en/dnd.json";

const catalog = {
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
