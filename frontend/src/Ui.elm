module Ui exposing
    ( derivationBadge
    , discSetCard
    , errorText
    , videoCard
    , viewPagination
    )

{-| Small shared view pieces used by the browse and search pages.
All presentation lives in the stylesheet; these emit classes only.
-}

import Api exposing (DerivationState(..), DiscSetEntry, IdentityState(..), VideoItem)
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onClick)
import Route


{-| A thumbnail card linking to the player.
-}
videoCard : VideoItem -> Html msg
videoCard item =
    div [ class "video-card" ]
        [ a
            [ href
                (Route.toString
                    (Route.Player { library = item.library, path = item.path })
                )
            ]
            [ cardThumbnail item
            , div [ class "card-title" ]
                [ text (Maybe.withDefault item.name item.title) ]
            ]
        , derivationBadge item
        ]


cardThumbnail : VideoItem -> Html msg
cardThumbnail item =
    case item.thumbPath of
        Just thumb ->
            img
                [ src (Api.thumbUrl item.library thumb)
                , alt item.name
                , class "card-thumb"
                ]
                []

        Nothing ->
            div [ class "card-thumb-placeholder" ] [ text "▶" ]


{-| A disc card: poster art once the disc is identified, else the main
title's frame thumbnail; the disc's title and year; and a note when
identification is still pending.
-}
discSetCard : DiscSetEntry -> Html msg
discSetCard set =
    div [ class "video-card" ]
        [ a
            [ href
                (Route.toString
                    (Route.Player
                        { library = set.main.library, path = set.main.path }
                    )
                )
            ]
            [ discThumbnail set
            , div [ class "card-title" ]
                (text set.displayTitle :: yearSuffix set.year)
            ]
        , derivationBadge set.main
        , identityBadge set
        ]


discThumbnail : DiscSetEntry -> Html msg
discThumbnail set =
    case set.posterPath of
        Just poster ->
            img
                [ src (Api.thumbUrl set.main.library poster)
                , alt set.displayTitle
                , class "card-thumb card-thumb-poster"
                ]
                []

        Nothing ->
            cardThumbnail set.main


yearSuffix : Maybe Int -> List (Html msg)
yearSuffix year =
    case year of
        Just y ->
            [ span [ class "card-year" ]
                [ text (" (" ++ String.fromInt y ++ ")") ]
            ]

        Nothing ->
            []


{-| A note for discs whose identity is not settled, linking to the rips
page where the operator can settle it; identified discs and legacy rips
(no identity state at all) show nothing.
-}
identityBadge : DiscSetEntry -> Html msg
identityBadge set =
    let
        label =
            case set.identityState of
                Just IdentityUnresolved ->
                    Just "🔍 needs identification"

                Just IdentityNeedsConfirmation ->
                    Just "🔍 needs identification"

                Just IdentityFailed ->
                    Just "⚠ identification failed"

                _ ->
                    Nothing
    in
    case label of
        Just badge ->
            a
                [ href (Route.toString Route.Rips)
                , class "card-badge card-badge-warn"
                ]
                [ text badge ]

        Nothing ->
            text ""


{-| A small status line for items without a ready browser-playable copy.
Ready states (not needed / done) show nothing.
-}
derivationBadge : VideoItem -> Html msg
derivationBadge item =
    let
        label =
            case item.derivation.state of
                NotNeeded ->
                    Nothing

                Done ->
                    Nothing

                Processing ->
                    Just "⚙ converting…"

                Pending ->
                    Just "⏳ conversion queued"

                Failed ->
                    Just "⚠ conversion failed"

                Unknown ->
                    Just "❓ format unknown"
    in
    case label of
        Just badge ->
            div [ class "card-badge" ] [ text badge ]

        Nothing ->
            text ""


errorText : String -> Html msg
errorText message =
    p [ class "error-note" ] [ text ("Error: " ++ message) ]


type PageItem
    = Page Int
    | Gap


{-| Build a list of page items with gaps where pages are skipped.
Always includes the first and last page, and pages within 2 of current.
-}
pageItems : Int -> Int -> List PageItem
pageItems current total =
    let
        shouldInclude i =
            i == 1 || i == total || abs (i - current) <= 2

        build i lastIncluded acc =
            if i > total then
                List.reverse acc

            else if shouldInclude i then
                let
                    gapped =
                        if lastIncluded >= 0 && i > lastIncluded + 1 then
                            Gap :: acc

                        else
                            acc
                in
                build (i + 1) i (Page i :: gapped)

            else
                build (i + 1) lastIncluded acc
    in
    build 1 -1 []


viewPagination : (Int -> msg) -> Int -> Int -> Html msg
viewPagination toMsg current total =
    div [ class "pagination" ]
        (List.map (viewPageItem toMsg current) (pageItems current total))


viewPageItem : (Int -> msg) -> Int -> PageItem -> Html msg
viewPageItem toMsg current item =
    case item of
        Gap ->
            span [ class "page-gap" ] [ text "…" ]

        Page p ->
            if p == current then
                span [ class "page-current" ] [ text (String.fromInt p) ]

            else
                button
                    [ onClick (toMsg p), class "page-button" ]
                    [ text (String.fromInt p) ]
