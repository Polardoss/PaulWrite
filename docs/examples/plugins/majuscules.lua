-- Met la sélection en majuscules (lettres ASCII seulement : string.upper de Lua ne connaît pas
-- les accents).
paul.command("exemple.majuscules", "Exemple : sélection en majuscules", function()
    local s = paul.selection()
    if s == "" then
        paul.message("Rien n'est sélectionné")
    else
        paul.insert(string.upper(s))
    end
end)
