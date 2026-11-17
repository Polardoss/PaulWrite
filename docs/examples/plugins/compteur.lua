-- Compte les mots de la sélection, ou du document entier s'il n'y a pas de sélection.
local function count_words(s)
    local n = 0
    for _ in string.gmatch(s, "%S+") do n = n + 1 end
    return n
end

paul.command("exemple.compter_mots", "Exemple : compter les mots", function()
    local s = paul.selection()
    if s ~= "" then
        paul.message(count_words(s) .. " mot(s) dans la sélection")
    else
        paul.message(count_words(paul.text()) .. " mot(s) dans le document, "
            .. paul.line_count() .. " ligne(s)")
    end
end)
