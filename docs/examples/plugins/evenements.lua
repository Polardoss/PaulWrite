-- Réagit aux événements de l'éditeur : « open », « save » et « close ».
paul.on("save", function(ev)
    paul.message("Enregistré : " .. (ev.path or "sans nom"))
end)
