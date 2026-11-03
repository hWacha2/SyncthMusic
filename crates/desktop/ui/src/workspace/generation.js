// Номер поколения данных рабочей области, общий для всех запросов. Ответ
// старого устройства, старой папки или прерванного действия не должен
// перетирать свежее состояние.
let generation = 0;

export function nextWorkspaceGeneration() {
  generation += 1;
  return generation;
}

export function isWorkspaceGeneration(value) {
  return value === generation;
}
